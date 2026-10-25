//! Blocks DNS (port 53) that would go around NetAgg while the user's own
//! servers are in force: systemd-resolved and apps can still ask a
//! network's servers directly, and a router's server is on-link, where the
//! capture routes don't reach. An nftables table of NetAgg's own; unlike
//! Windows' WFP session it outlives a crash, so the service also removes it
//! at start and when systemd stops it (`--clean-up`).

use std::io::{self, Write};
use std::process::{Command, Stdio};

use super::adapters::interface_name;

const TABLE: &str = "netagg_dns";

/// The table, removed when dropped.
pub struct DnsGuard;

impl DnsGuard {
    /// Lets port 53 out only through the adapter (`tun_index`), on loopback
    /// and from NetAgg's own cgroup (its upstream queries). Needs root.
    pub fn apply(tun_index: u32) -> io::Result<DnsGuard> {
        let tun_name = interface_name(tun_index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the adapter is gone"))?;
        let cgroup = own_cgroup()?;
        nft(&ruleset(&tun_name, &cgroup))?;
        Ok(DnsGuard)
    }

    /// Deletes a table a run that didn't stop cleanly left. Returns true if
    /// there was one.
    pub fn remove_stale() -> bool {
        match nft(&format!("delete table inet {TABLE}\n")) {
            Ok(()) => true,
            // None there: the usual case.
            Err(err) if err.to_string().contains("No such file or directory") => false,
            Err(err) => {
                log::warn!("couldn't remove a stale DNS block: {err}");
                false
            }
        }
    }
}

impl Drop for DnsGuard {
    fn drop(&mut self) {
        if let Err(err) = nft(&format!("delete table inet {TABLE}\n")) {
            log::warn!("couldn't remove the DNS block: {err}");
        }
    }
}

/// This process's cgroup (v2), as `/proc/self/cgroup` has it: under
/// systemd, `system.slice/netagg.service`.
fn own_cgroup() -> io::Result<String> {
    let text = std::fs::read_to_string("/proc/self/cgroup")?;
    parse_cgroup(&text).ok_or_else(|| io::Error::other("no cgroup v2 for NetAgg"))
}

fn parse_cgroup(text: &str) -> Option<String> {
    let path = text.lines().find_map(|line| line.strip_prefix("0::/"))?;
    // A name nft would need to escape, or the root (which matches everything).
    let plain = |c: char| c.is_ascii_alphanumeric() || "._-@/:".contains(c);
    (!path.is_empty() && path.chars().all(plain)).then(|| path.to_owned())
}

/// Replaces the table in one transaction, so there's never a moment with
/// half the rules.
fn ruleset(tun_name: &str, cgroup: &str) -> String {
    let level = cgroup.split('/').count();
    format!(
        "table inet {TABLE}\n\
         delete table inet {TABLE}\n\
         table inet {TABLE} {{\n\
         \tchain output {{\n\
         \t\ttype filter hook output priority filter; policy accept;\n\
         \t\toifname \"lo\" accept\n\
         \t\toifname \"{tun_name}\" accept\n\
         \t\tsocket cgroupv2 level {level} \"{cgroup}\" accept\n\
         \t\tmeta l4proto {{ udp, tcp }} th dport 53 counter reject\n\
         \t}}\n\
         }}\n"
    )
}

fn nft(script: &str) -> io::Result<()> {
    let mut child = Command::new("nft")
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), format!("couldn't run nft: {err}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("nft has no input"))?
        .write_all(script.as_bytes())?;
    let out = child.wait_with_output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "nft: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_service_cgroup() {
        assert_eq!(
            parse_cgroup("0::/system.slice/netagg.service\n").as_deref(),
            Some("system.slice/netagg.service")
        );
        // A user session, as in the lab.
        assert_eq!(
            parse_cgroup("0::/user.slice/user-1002.slice/session-4.scope\n").as_deref(),
            Some("user.slice/user-1002.slice/session-4.scope")
        );
        assert_eq!(parse_cgroup("0::/\n"), None);
        assert_eq!(parse_cgroup("12:cpu:/x\n"), None);
        assert_eq!(parse_cgroup("0::/a\"b\n"), None);
    }

    #[test]
    fn blocks_port_53_except_through_netagg() {
        let rules = ruleset("NetAgg", "system.slice/netagg.service");
        assert!(rules.starts_with("table inet netagg_dns\ndelete table inet netagg_dns\n"));
        assert!(rules.contains("oifname \"NetAgg\" accept"));
        assert!(rules.contains("socket cgroupv2 level 2 \"system.slice/netagg.service\" accept"));
        // Counted, so `nft list table inet netagg_dns` shows what it stopped.
        assert!(rules.contains("th dport 53 counter reject"));
        // The exceptions come before the block.
        assert!(rules.find("cgroupv2").unwrap() < rules.find("reject").unwrap());
    }
}
