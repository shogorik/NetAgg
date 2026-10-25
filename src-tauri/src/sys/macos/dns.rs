use std::io;
use std::net::IpAddr;

use super::adapters::interface_name;
use super::command::{self, Cmd, SCUTIL};

/// NetAgg's entry in configd's dynamic store. It lives in memory only
/// (gone at reboot) and nothing in the saved network settings changes.
const KEY: &str = "State:/Network/Service/dev.netagg.app/DNS";

/// Sends lookups to `servers` through the adapter while NetAgg is on, as a
/// supplemental resolver for every domain (match domain `""`), the way VPN
/// clients do. Other resolvers (a VPN's own, Tailscale's) stay as they were.
/// Removed again when dropped.
pub struct ResolverOverride;

impl ResolverOverride {
    pub fn apply(if_index: u32, servers: &[IpAddr]) -> io::Result<ResolverOverride> {
        if servers.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no DNS servers to set",
            ));
        }
        let interface = interface_name(if_index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the adapter is gone"))?;
        command::change(
            &Cmd::new(SCUTIL, &[] as &[&str]).with_stdin(set_script(&interface, servers)),
        )?;
        Ok(ResolverOverride)
    }

    /// Removes the entry a run that didn't stop cleanly left behind. Returns
    /// true if there was one. Harmless when there isn't.
    pub fn remove_stale() -> bool {
        let shown = command::query(&Cmd::new(SCUTIL, &[] as &[&str]).with_stdin(show_script()));
        let present = shown.is_ok_and(|out| is_present(&out));
        if present {
            if let Err(err) = remove() {
                log::warn!("{err}");
                return false;
            }
        }
        present
    }
}

impl Drop for ResolverOverride {
    fn drop(&mut self) {
        if let Err(err) = remove() {
            log::warn!("couldn't remove the DNS servers: {err}");
        }
    }
}

fn remove() -> io::Result<()> {
    command::change(&Cmd::new(SCUTIL, &[] as &[&str]).with_stdin(format!("remove {KEY}\nquit\n")))
        .map(|_| ())
}

/// An `scutil` script that writes the resolver: its servers, every domain,
/// and the adapter its queries go through.
fn set_script(interface: &str, servers: &[IpAddr]) -> String {
    let servers: Vec<String> = servers.iter().map(IpAddr::to_string).collect();
    format!(
        "d.init\n\
         d.add ServerAddresses * {}\n\
         d.add SupplementalMatchDomains * \"\"\n\
         d.add InterfaceName {interface}\n\
         set {KEY}\n\
         quit\n",
        servers.join(" ")
    )
}

/// The network's own DNS servers, from the resolvers `scutil --dns` scopes
/// to its interface.
pub fn link_dns_servers(if_index: u32) -> Vec<IpAddr> {
    command::query(&Cmd::new(SCUTIL, &["--dns"]))
        .map(|out| parse_scutil_dns(&out, if_index))
        .unwrap_or_default()
}

/// Each `resolver #N` block lists `nameserver[i] : ip` lines and, when it
/// belongs to one interface, `if_index : N (enX)`.
fn parse_scutil_dns(out: &str, if_index: u32) -> Vec<IpAddr> {
    let mut found = Vec::new();
    for block in out.split("resolver #").skip(1) {
        let field = |line: &str, key: &str| {
            let (k, v) = line.split_once(':')?;
            k.trim().starts_with(key).then(|| v.trim().to_owned())
        };
        let index = block.lines().find_map(|l| field(l, "if_index"));
        let ours = index
            .and_then(|v| v.split_whitespace().next()?.parse::<u32>().ok())
            .is_some_and(|i| i == if_index);
        if !ours {
            continue;
        }
        for ip in block.lines().filter_map(|l| field(l, "nameserver")) {
            // The resolver adds the scope back from the interface.
            if let Ok(ip) = ip.split('%').next().unwrap_or_default().parse() {
                if !found.contains(&ip) {
                    found.push(ip);
                }
            }
        }
    }
    found
}

fn show_script() -> String {
    format!("show {KEY}\nquit\n")
}

/// `scutil` prints "No such key" for a key that isn't there, and the
/// dictionary otherwise.
fn is_present(shown: &str) -> bool {
    !shown.contains("No such key") && shown.contains("ServerAddresses")
}

#[cfg(test)]
mod tests {
    use super::super::command::fake;
    use super::*;

    #[test]
    fn reads_an_interfaces_servers() {
        let out = "DNS configuration\n\n\
            resolver #1\n  search domain[0] : lan\n  nameserver[0] : 192.168.1.1\n  if_index : 6 (en0)\n  reach : 0x00020002\n\n\
            resolver #2\n  domain   : local\n  options  : mdns\n\n\
            DNS configuration (for scoped queries)\n\n\
            resolver #1\n  nameserver[0] : 192.168.1.1\n  nameserver[1] : fe80::1%en0\n  if_index : 6 (en0)\n\n\
            resolver #2\n  nameserver[0] : 172.20.10.1\n  if_index : 14 (en8)\n";
        let servers: Vec<String> = parse_scutil_dns(out, 6)
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(servers, ["192.168.1.1", "fe80::1"]);
        assert_eq!(
            parse_scutil_dns(out, 14),
            ["172.20.10.1".parse::<IpAddr>().unwrap()]
        );
        assert!(parse_scutil_dns(out, 99).is_empty());
    }

    #[test]
    fn writes_a_supplemental_resolver() {
        let servers = [
            "8.8.8.8".parse().unwrap(),
            "2606:4700::1111".parse().unwrap(),
        ];
        assert_eq!(
            set_script("utun5", &servers),
            "d.init\n\
             d.add ServerAddresses * 8.8.8.8 2606:4700::1111\n\
             d.add SupplementalMatchDomains * \"\"\n\
             d.add InterfaceName utun5\n\
             set State:/Network/Service/dev.netagg.app/DNS\n\
             quit\n"
        );
    }

    #[test]
    fn applies_then_removes_on_drop() {
        let fake = fake::succeed();
        let guard = ResolverOverride::apply(1, &["1.1.1.1".parse().unwrap()]).unwrap();
        drop(guard);
        let calls = fake.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0]
            .stdin
            .as_deref()
            .unwrap()
            .contains("d.add ServerAddresses * 1.1.1.1"));
        assert!(calls[0]
            .stdin
            .as_deref()
            .unwrap()
            .contains("InterfaceName lo0"));
        assert_eq!(
            calls[1].stdin.as_deref(),
            Some("remove State:/Network/Service/dev.netagg.app/DNS\nquit\n")
        );
    }

    #[test]
    fn needs_servers_and_an_adapter() {
        let fake = fake::succeed();
        assert!(ResolverOverride::apply(1, &[]).is_err());
        assert!(ResolverOverride::apply(u32::MAX, &["1.1.1.1".parse().unwrap()]).is_err());
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn stale_entries_are_removed_once() {
        let mut stored = true;
        let fake = fake::install(move |cmd| {
            let script = cmd.stdin.clone().unwrap_or_default();
            if script.starts_with("show") {
                Ok(if stored {
                    "<dictionary> {\n  ServerAddresses : <array> {\n    0 : 8.8.8.8\n  }\n}\n"
                        .into()
                } else {
                    "  No such key\n".into()
                })
            } else {
                stored = false;
                Ok(String::new())
            }
        });
        assert!(ResolverOverride::remove_stale());
        assert!(!ResolverOverride::remove_stale());
        // show, remove, show.
        assert_eq!(fake.calls().len(), 3);
    }
}
