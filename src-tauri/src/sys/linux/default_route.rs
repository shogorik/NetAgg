//! Which network Linux routes through by default: the default route with
//! the lowest metric. NetAgg adds its own default route through the chosen
//! network, below any metric NetworkManager or systemd-networkd use, and
//! marks it with a route protocol of its own, so a later run finds and
//! removes it without having to remember anything. Like Windows' active
//! store, it's gone after a reboot.

use std::io;
use std::process::Command;

use serde::Deserialize;

use super::adapters::interface_name;

/// NetAgg's routes, as `ip route` shows them (`proto 78`).
const PROTOCOL: &str = "78";
/// NetworkManager starts at 100 (wired) and 600 (Wi-Fi), networkd at 1024.
const METRIC: &str = "5";

/// A default route as `ip -j route` lists it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct DefaultRoute {
    dev: String,
    gateway: Option<String>,
    metric: Option<u32>,
}

impl DefaultRoute {
    fn same_path(&self, other: &DefaultRoute) -> bool {
        self.dev == other.dev && self.gateway == other.gateway
    }
}

/// Makes `if_index` the network Linux prefers, for IPv4 and IPv6, through
/// that network's own gateway, and removes NetAgg's default routes through
/// anything else. `None` only removes. Needs root (or `CAP_NET_ADMIN`).
pub fn prefer_interface(if_index: Option<u32>) -> io::Result<()> {
    let name = if_index.and_then(interface_name);
    let mut first_err = None;
    for family in ["-4", "-6"] {
        if let Err(err) = prefer_for(family, name.as_deref()) {
            first_err.get_or_insert(err);
        }
    }
    first_err.map_or(Ok(()), Err)
}

fn prefer_for(family: &str, name: Option<&str>) -> io::Result<()> {
    // Not filtered by device: `ip` then leaves the device out of its JSON.
    let all = routes(family, &[])?;
    let ours = routes(family, &["proto", PROTOCOL])?;
    let (remove, add) = plan(&all, &ours, name);
    for route in &remove {
        ip(family, "del", route)?;
    }
    match add {
        Some(route) => ip(family, "add", &route),
        None => Ok(()),
    }
}

/// Which of NetAgg's routes go, and which one comes: through `name`, by the
/// gateway of that network's own default route.
fn plan(
    all: &[DefaultRoute],
    ours: &[DefaultRoute],
    name: Option<&str>,
) -> (Vec<DefaultRoute>, Option<DefaultRoute>) {
    let wanted = name.and_then(|name| {
        all.iter()
            .find(|r| r.dev == name && r.gateway.is_some() && !ours.contains(r))
    });
    let remove = ours
        .iter()
        .filter(|r| !wanted.is_some_and(|w| w.same_path(r)))
        .cloned()
        .collect();
    let add = wanted
        .filter(|w| !ours.iter().any(|r| r.same_path(w)))
        .cloned();
    (remove, add)
}

/// The default routes `filter` selects (`ip route show default …`).
fn routes(family: &str, filter: &[&str]) -> io::Result<Vec<DefaultRoute>> {
    let out = Command::new("ip")
        .args(["-j", family, "route", "show", "default"])
        .args(filter)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    parse_routes(&out.stdout)
}

fn parse_routes(json: &[u8]) -> io::Result<Vec<DefaultRoute>> {
    // An empty table prints nothing at all.
    if json.iter().all(u8::is_ascii_whitespace) {
        return Ok(Vec::new());
    }
    serde_json::from_slice(json).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// `ip route <add|del> default via <gateway> dev <dev> proto 78 metric 5`.
fn ip(family: &str, action: &str, route: &DefaultRoute) -> io::Result<()> {
    let out = Command::new("ip")
        .args([family, "route", action, "default"])
        .args(route_args(route))
        .output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "ip route {action} default via {:?} dev {}: {}",
            route.gateway,
            route.dev,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

fn route_args(route: &DefaultRoute) -> Vec<&str> {
    let mut args = Vec::new();
    if let Some(gateway) = &route.gateway {
        args.extend(["via", gateway.as_str()]);
    }
    args.extend([
        "dev",
        route.dev.as_str(),
        "proto",
        PROTOCOL,
        "metric",
        METRIC,
    ]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(dev: &str, gateway: Option<&str>, metric: Option<u32>) -> DefaultRoute {
        DefaultRoute {
            dev: dev.into(),
            gateway: gateway.map(Into::into),
            metric,
        }
    }

    #[test]
    fn adds_once_keeps_and_moves_the_route() {
        let wan1 = route("wan1", Some("192.168.11.1"), Some(100));
        let wan2 = route("wan2", Some("192.168.12.1"), Some(200));
        let ours2 = route("wan2", Some("192.168.12.1"), Some(5));
        let ours1 = route("wan1", Some("192.168.11.1"), Some(5));

        // First time: added through wan2's own gateway.
        let (remove, add) = plan(&[wan1.clone(), wan2.clone()], &[], Some("wan2"));
        assert!(remove.is_empty());
        assert_eq!(add.as_ref().map(|r| r.dev.as_str()), Some("wan2"));
        assert_eq!(add.unwrap().gateway.as_deref(), Some("192.168.12.1"));

        // Already there: nothing to do, though ours shares the gateway.
        let all = [ours2.clone(), wan1.clone(), wan2.clone()];
        assert_eq!(
            plan(&all, std::slice::from_ref(&ours2), Some("wan2")),
            (vec![], None)
        );

        // Moved to wan1: ours through wan2 goes, one through wan1 comes.
        let (remove, add) = plan(&all, std::slice::from_ref(&ours2), Some("wan1"));
        assert_eq!(remove, std::slice::from_ref(&ours2));
        assert_eq!(add.unwrap().dev, "wan1");

        // Automatic, or the network lost its own route: ours goes.
        let all = [ours1.clone(), wan1.clone()];
        assert_eq!(
            plan(&all, std::slice::from_ref(&ours1), None),
            (vec![ours1.clone()], None)
        );
        assert_eq!(
            plan(&all, std::slice::from_ref(&ours1), Some("wan2")),
            (vec![ours1], None)
        );
    }

    #[test]
    fn reads_ip_json() {
        let json = br#"[{"dst":"default","gateway":"198.51.100.1","dev":"wlp4s0","protocol":"dhcp","prefsrc":"198.51.100.70","metric":600,"flags":[]},{"dst":"default","dev":"wg0","flags":[]}]"#;
        let routes = parse_routes(json).unwrap();
        assert_eq!(
            routes,
            [
                route("wlp4s0", Some("198.51.100.1"), Some(600)),
                route("wg0", None, None)
            ]
        );
        assert!(parse_routes(b"\n").unwrap().is_empty());
        assert!(parse_routes(b"[]").unwrap().is_empty());
    }

    #[test]
    fn builds_a_marked_route() {
        let route = route("enp5s0", Some("fe80::1"), Some(100));
        assert_eq!(
            route_args(&route),
            ["via", "fe80::1", "dev", "enp5s0", "proto", "78", "metric", "5"]
        );
    }
}
