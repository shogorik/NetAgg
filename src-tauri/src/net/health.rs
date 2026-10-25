//! Keeps checking that every network can actually reach the internet, so a
//! Wi-Fi with no backhaul or an unplugged phone drops out of rotation.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio::task::JoinSet;

use super::dial;
use super::links::{Diagnosis, Link, LinkInfo, Links};
use crate::sys;

pub const PROBE_INTERVAL: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Anycast HTTPS endpoints, Cloudflare then AdGuard. A TCP handshake is
/// enough: it proves the path works without sending anything.
const TARGETS_V4: [&str; 2] = ["1.1.1.1:443", "94.140.14.14:443"];
const TARGETS_V6: [&str; 2] = ["[2606:4700:4700::1111]:443", "[2a10:50c0::ad1:ff]:443"];
/// For the router and provider stages, which are local or one hop away.
const ICMP_TIMEOUT_MS: u32 = 1000;

/// Round-trip time of a TCP handshake through `link`, or `None` if no target
/// answered. Uses IPv4 when the link has it, as almost every flow does.
pub async fn probe(link: &Link) -> Option<u32> {
    let info = link.info();
    if info.ipv4.is_some() {
        reach(&info, TARGETS_V4).await
    } else {
        reach(&info, TARGETS_V6).await
    }
}

/// Whether IPv6 works through `link`, probed on its own because it can
/// fail while IPv4 is fine. `None` for an IPv6-only link, which `probe`
/// already answers for.
async fn probe_ipv6(link: &Link) -> Option<bool> {
    let info = link.info();
    match (info.ipv4, info.ipv6) {
        (_, None) => Some(false),
        (None, Some(_)) => None,
        (Some(_), Some(_)) => Some(reach(&info, TARGETS_V6).await.is_some()),
    }
}

/// Probes `link` stage by stage and records the result. The link counts as
/// up when either internet target answers.
async fn check(link: &Link) {
    let (diagnosis, ipv6_ok) = tokio::join!(diagnose(link), probe_ipv6(link));
    let rtt = match (diagnosis.cloudflare, diagnosis.adguard) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    *link.diagnosis.lock().unwrap() = diagnosis;
    link.set_ipv6_ok(ipv6_ok.unwrap_or(rtt.is_some()));
    match rtt {
        Some(ms) => link.record_success(Some(ms)),
        None => {
            link.record_failure();
            // One miss can be a lost packet. Asking again at once, rather
            // than after the interval, takes a dead link out in seconds.
            if link.usable() {
                match probe(link).await {
                    Some(ms) => link.record_success(Some(ms)),
                    None => link.record_failure(),
                }
            }
        }
    }
}

/// Every stage of `link`'s path, side by side.
async fn diagnose(link: &Link) -> Diagnosis {
    let info = link.info();
    let targets = if info.ipv4.is_some() {
        TARGETS_V4
    } else {
        TARGETS_V6
    };
    let local = local_stages(&info);
    let (cloudflare, adguard, (router, isp)) = tokio::join!(
        reach(&info, [targets[0]; 1]),
        reach(&info, [targets[1]; 1]),
        local,
    );
    let (isp, isp_address) = isp.map_or((None, None), |(ms, ip)| (Some(ms), Some(ip)));
    Diagnosis {
        router,
        isp,
        isp_address,
        cloudflare,
        adguard,
    }
}

/// The gateway ping and the first hop past it, over IPv4 (ICMP, blocking,
/// so on the blocking pool).
async fn local_stages(info: &LinkInfo) -> (Option<u32>, Option<(u32, Ipv4Addr)>) {
    let (Some(source), Some(gateway)) = (
        info.ipv4,
        info.gateway
            .as_deref()
            .and_then(|g| g.parse::<Ipv4Addr>().ok()),
    ) else {
        return (None, None);
    };
    let if_index = info.if_index;
    tokio::task::spawn_blocking(move || {
        let timed = |f: &dyn Fn() -> bool| {
            let start = Instant::now();
            f().then(|| start.elapsed().as_millis().min(u32::MAX as u128) as u32)
        };
        let router = timed(&|| {
            sys::icmp_echo(if_index, source, gateway, b"netagg", 64, ICMP_TIMEOUT_MS).is_some()
        });
        let start = Instant::now();
        let isp = sys::icmp_hop(source, Ipv4Addr::new(1, 1, 1, 1), 2, ICMP_TIMEOUT_MS)
            .map(|ip| (start.elapsed().as_millis().min(u32::MAX as u128) as u32, ip));
        (router, isp)
    })
    .await
    .unwrap_or((None, None))
}

async fn reach<const N: usize>(info: &LinkInfo, targets: [&str; N]) -> Option<u32> {
    for target in targets {
        let dest: SocketAddr = target.parse().expect("valid probe target");
        let start = Instant::now();
        if dial::tcp(info, dest, PROBE_TIMEOUT).await.is_ok() {
            return Some(start.elapsed().as_millis().min(u32::MAX as u128) as u32);
        }
    }
    None
}

/// Probes every present link, side by side, until `stop` turns true.
pub async fn run(links: Arc<Links>, mut stop: watch::Receiver<bool>) {
    loop {
        let mut probes = JoinSet::new();
        for link in links.present() {
            probes.spawn(async move { check(&link).await });
        }
        while probes.join_next().await.is_some() {}

        tokio::select! {
            _ = tokio::time::sleep(PROBE_INTERVAL) => {}
            // A dropped sender means the engine is gone.
            changed = stop.changed() => if changed.is_err() { return },
        }
        if *stop.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every stage for every network on this machine. Run with
    /// `cargo test live_diagnose -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_diagnose() {
        let links = Links::default();
        links.refresh();
        for link in links.present() {
            println!("{}: {:?}", link.info().name, diagnose(&link).await);
        }
    }

    /// Probes every network on this machine through a pinned socket. Run
    /// with `cargo test live_probe -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_probe() {
        let links = Links::default();
        links.refresh();
        for link in links.present() {
            let rtt = probe(&link).await;
            let info = link.info();
            println!(
                "{} (if {}, {:?}): {rtt:?} ms",
                info.name, info.if_index, info.ipv4
            );
            assert!(rtt.is_some(), "{} could not reach the internet", link.id);
        }
    }
}
