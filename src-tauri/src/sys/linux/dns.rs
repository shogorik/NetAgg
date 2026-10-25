use std::io;
use std::net::IpAddr;
use std::process::Command;

use super::adapters::interface_name;

/// Points the adapter at `servers` and makes it the DNS route for every
/// domain (`~.`), so lookups go through NetAgg too. systemd-resolved
/// forgets both once the adapter is gone.
///
/// Only root may: resolved asks anyone else for a password through polkit,
/// capabilities or not. Without it DNS stays with the system resolver, which
/// still works: its queries go out on-link, or bound to a real network.
pub fn set_dns(if_index: u32, servers: &[IpAddr]) -> io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "only root may change systemd-resolved's settings",
        ));
    }
    let name = interface_name(if_index)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the adapter is gone"))?;
    let mut dns = vec!["dns".to_owned(), name.clone()];
    dns.extend(servers.iter().map(IpAddr::to_string));
    resolvectl(&dns)?;
    resolvectl(&["domain".to_owned(), name, "~.".to_owned()])
}

/// The network's own DNS servers: systemd-resolved's per-link list, or else
/// the nameservers in /etc/resolv.conf (which belong to no one network).
pub fn link_dns_servers(if_index: u32) -> Vec<IpAddr> {
    let from_resolved = interface_name(if_index).and_then(|name| {
        let out = Command::new("resolvectl")
            .args(["dns", &name])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| parse_resolvectl_dns(&String::from_utf8_lossy(&out.stdout)))
    });
    match from_resolved {
        Some(servers) if !servers.is_empty() => servers,
        _ => std::fs::read_to_string("/etc/resolv.conf")
            .map(|conf| parse_resolv_conf(&conf))
            .unwrap_or_default(),
    }
}

/// `Link 3 (wlan0): 192.168.1.1 fe80::1%wlan0 9.9.9.9#dns.quad9.net`
fn parse_resolvectl_dns(out: &str) -> Vec<IpAddr> {
    out.lines()
        .filter_map(|line| line.split_once("): "))
        .flat_map(|(_, list)| list.split_whitespace())
        .filter_map(|server| server.split(['%', '#']).next()?.parse().ok())
        .collect()
}

fn parse_resolv_conf(conf: &str) -> Vec<IpAddr> {
    conf.lines()
        .filter_map(|line| line.trim().strip_prefix("nameserver"))
        .filter_map(|ip| ip.trim().split('%').next()?.parse().ok())
        .collect()
}

fn resolvectl(args: &[String]) -> io::Result<()> {
    let out = Command::new("resolvectl").args(args).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "resolvectl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_resolved_link_servers() {
        let out = "Link 3 (wlan0): 192.168.1.1 fe80::1%wlan0 9.9.9.9#dns.quad9.net\n";
        let servers: Vec<String> = parse_resolvectl_dns(out)
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(servers, ["192.168.1.1", "fe80::1", "9.9.9.9"]);
        assert!(parse_resolvectl_dns("Link 4 (usb0):\n").is_empty());
    }

    #[test]
    fn reads_resolv_conf() {
        let conf = "# generated\nnameserver 127.0.0.53\nnameserver fe80::1%eth0\nsearch lan\n";
        let servers: Vec<String> = parse_resolv_conf(conf)
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(servers, ["127.0.0.53", "fe80::1"]);
    }
}
