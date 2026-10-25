//! The physical networks NetAgg can send traffic through.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use netdev::interface::state::OperState;
use netdev::interface::types::InterfaceType;

use crate::engine;
use crate::sys;

/// Seconds of throughput history kept for the charts.
pub const HISTORY_LEN: usize = 60;
/// A flow that downloads at least this much in a second is busy.
const BUSY_BYTES: u64 = 16 * 1024;
/// New flows count as busy for this long, before there is a rate to go by,
/// so a burst of new connections is spread out instead of all landing on
/// one link.
const FRESH: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct LinkInfo {
    pub if_index: u32,
    pub name: String,
    pub description: String,
    pub kind: &'static str,
    pub ipv4: Option<Ipv4Addr>,
    pub ipv6: Option<Ipv6Addr>,
    pub gateway: Option<String>,
    pub speed_bps: Option<u64>,
    /// Connected, with an address and a default gateway.
    pub present: bool,
}

/// One check of a link's path, stage by stage, in milliseconds (`None`: no
/// answer). Narrows a problem down: the local link, the router's uplink, or
/// further out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diagnosis {
    /// A ping to the link's gateway (the router, or the phone).
    pub router: Option<u32>,
    /// The first hop past the gateway: the provider's side of the uplink.
    pub isp: Option<u32>,
    pub isp_address: Option<Ipv4Addr>,
    /// TCP handshakes on port 443.
    pub cloudflare: Option<u32>,
    pub adguard: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthState {
    Unknown,
    Up,
    Down,
}

#[derive(Debug)]
pub struct Health {
    pub state: HealthState,
    pub latency_ms: Option<u32>,
    pub consecutive_failures: u32,
    /// An IPv6 probe got through. Having an address isn't enough: a phone
    /// can keep a stale prefix after its own IPv6 has gone.
    pub ipv6_ok: bool,
}

impl Health {
    fn unknown() -> Health {
        Health {
            state: HealthState::Unknown,
            latency_ms: None,
            consecutive_failures: 0,
            ipv6_ok: false,
        }
    }
}

#[derive(Default)]
pub struct LinkStats {
    pub rx: AtomicU64,
    pub tx: AtomicU64,
    pub tcp_active: AtomicU32,
    pub udp_active: AtomicU32,
    pub opened: AtomicU64,
    pub failed: AtomicU64,
    /// Highest recent download rate in bytes/s, decaying slowly. Drives
    /// adaptive weighting.
    pub peak_rx: AtomicU64,
    /// The best download rate the link has shown, bytes/s, fading to half
    /// in an hour (so a network that really slowed down is noticed): what it can carry, for when it has nothing to show right
    /// now (idle, or just after a restart). Saved with the settings.
    pub capacity: AtomicU64,
    /// Flows that were busy at the last tick.
    pub busy: AtomicU32,
    /// Flows opened since the last tick; they're busy until it sees them.
    pub opened_since_tick: AtomicU32,
}

/// One flow's download counter, so a tick can tell busy flows from idle ones.
pub struct Meter {
    rx: AtomicU64,
    seen_rx: AtomicU64,
    opened: Instant,
}

impl Meter {
    pub fn add_rx(&self, n: u64) {
        self.rx.fetch_add(n, Ordering::Relaxed);
    }
}

pub struct Rates {
    pub last_rx: u64,
    pub last_tx: u64,
    pub rx_rate: u64,
    pub tx_rate: u64,
    pub history: VecDeque<(u64, u64)>,
}

pub struct Link {
    pub id: String,
    pub info: RwLock<LinkInfo>,
    pub stats: LinkStats,
    pub health: Mutex<Health>,
    pub rates: Mutex<Rates>,
    /// Where along its path the link answered at the last check.
    pub diagnosis: Mutex<Diagnosis>,
    /// Another link whose router this one's path goes through (a phone
    /// tethering the same Wi-Fi), so both share one internet line.
    pub shares_line_with: Mutex<Option<String>>,
    meters: Mutex<Vec<Weak<Meter>>>,
}

impl Link {
    pub fn new(id: String, info: LinkInfo) -> Link {
        Link {
            id,
            info: RwLock::new(info),
            stats: LinkStats::default(),
            health: Mutex::new(Health::unknown()),
            rates: Mutex::new(Rates {
                last_rx: 0,
                last_tx: 0,
                rx_rate: 0,
                tx_rate: 0,
                history: VecDeque::from(vec![(0, 0); HISTORY_LEN]),
            }),
            diagnosis: Mutex::default(),
            shares_line_with: Mutex::new(None),
            meters: Mutex::new(Vec::new()),
        }
    }

    /// Starts metering a new flow on this link, for as long as the meter lives.
    pub fn meter(&self) -> Arc<Meter> {
        let meter = Arc::new(Meter {
            rx: AtomicU64::new(0),
            seen_rx: AtomicU64::new(0),
            opened: Instant::now(),
        });
        self.meters.lock().unwrap().push(Arc::downgrade(&meter));
        self.stats.opened_since_tick.fetch_add(1, Ordering::Relaxed);
        meter
    }

    /// What the balancer should expect the link to carry: the most it has
    /// shown, lately or over the last hour or so. Not the recent peak alone
    /// while it's busy: at the start of a burst its first flows are still
    /// ramping up, and their low rate would send the next flows to a slower
    /// network that happens to be idle.
    pub fn capacity_estimate(&self) -> u64 {
        let peak = self.stats.peak_rx.load(Ordering::Relaxed);
        peak.max(self.stats.capacity.load(Ordering::Relaxed))
    }

    /// Busy flows right now, for the balancer.
    pub fn busy(&self) -> u32 {
        self.stats.busy.load(Ordering::Relaxed)
            + self.stats.opened_since_tick.load(Ordering::Relaxed)
    }

    /// Counts the flows that moved data since the last call, or are new,
    /// and forgets closed ones.
    fn count_busy(&self, now: Instant) -> u32 {
        self.stats.opened_since_tick.store(0, Ordering::Relaxed);
        let mut busy = 0;
        self.meters.lock().unwrap().retain(|weak| {
            let Some(meter) = weak.upgrade() else {
                return false;
            };
            let rx = meter.rx.load(Ordering::Relaxed);
            let moved = rx - meter.seen_rx.swap(rx, Ordering::Relaxed);
            if moved >= BUSY_BYTES || now.duration_since(meter.opened) < FRESH {
                busy += 1;
            }
            true
        });
        busy
    }

    pub fn info(&self) -> LinkInfo {
        self.info.read().unwrap().clone()
    }

    pub fn usable(&self) -> bool {
        self.info.read().unwrap().present && self.health.lock().unwrap().state != HealthState::Down
    }

    /// Usable for IPv6 flows: has an address, and its IPv6 was seen working.
    pub fn carries_ipv6(&self) -> bool {
        self.info.read().unwrap().ipv6.is_some() && self.health.lock().unwrap().ipv6_ok
    }

    pub fn set_ipv6_ok(&self, ok: bool) {
        self.health.lock().unwrap().ipv6_ok = ok;
    }

    pub fn record_success(&self, latency_ms: Option<u32>) {
        let mut h = self.health.lock().unwrap();
        h.state = HealthState::Up;
        h.consecutive_failures = 0;
        if latency_ms.is_some() {
            h.latency_ms = latency_ms;
        }
    }

    /// Two failures in a row take a network out of rotation until it answers again.
    pub fn record_failure(&self) {
        let mut h = self.health.lock().unwrap();
        h.consecutive_failures += 1;
        if h.consecutive_failures >= 2 {
            h.state = HealthState::Down;
            h.latency_ms = None;
        }
    }
}

/// Where the probes for shared lines go: any address past the routers.
const PATH_PROBE: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

/// The link whose router appears among `hops` (a path's second and third
/// hops), other than the path's own gateway: that path then reaches the
/// internet through the other link's router.
fn shared_with<'a>(
    hops: &[Ipv4Addr],
    own_gateway: Option<&str>,
    others: &[(&'a str, &str)],
) -> Option<&'a str> {
    hops.iter()
        .map(|hop| hop.to_string())
        .filter(|hop| Some(hop.as_str()) != own_gateway)
        .find_map(|hop| others.iter().find(|(_, gw)| *gw == hop).map(|(id, _)| *id))
}

/// Marks every present link whose path runs through another link's router.
/// Sends two short-lived pings per link and waits up to a second for each.
/// Blocking.
pub fn check_shared_lines(links: &Links) {
    let present = links.present();
    let gateways: Vec<(String, String)> = present
        .iter()
        .filter_map(|l| Some((l.id.clone(), l.info().gateway?)))
        .collect();
    for link in &present {
        let info = link.info();
        let found = info.ipv4.and_then(|source| {
            let hops: Vec<Ipv4Addr> = (2..=3)
                .filter_map(|ttl| sys::icmp_hop(source, PATH_PROBE, ttl, 1000))
                .collect();
            let others: Vec<(&str, &str)> = gateways
                .iter()
                .filter(|(id, _)| *id != link.id)
                .map(|(id, gw)| (id.as_str(), gw.as_str()))
                .collect();
            shared_with(&hops, info.gateway.as_deref(), &others).map(str::to_owned)
        });
        let mut shares = link.shares_line_with.lock().unwrap();
        if *shares != found {
            log::info!("{}: shares a line with {found:?}", link.id);
            *shares = found;
        }
    }
}

impl LinkInfo {
    /// What to call the network when the user hasn't named it. Windows
    /// calls a USB-tethered phone "Ethernet 2" and Linux "usb0", which says
    /// nothing about it being a phone and its data.
    pub fn display_name(&self) -> String {
        let says_phone = ["phone", "android", "pixel", "galaxy"]
            .iter()
            .any(|n| self.name.to_lowercase().contains(n));
        if self.kind == "phone" && !says_phone {
            "Phone (USB tethering)".to_owned()
        } else {
            self.name.clone()
        }
    }

    /// A phone's or mobile network's data, usually limited.
    fn metered(&self) -> bool {
        matches!(self.kind, "phone" | "cellular")
    }

    /// Which link takes a flow when everything else ties: wired, Wi-Fi,
    /// other, then metered.
    pub fn rank(&self) -> u8 {
        match self.kind {
            "ethernet" => 0,
            "wifi" => 1,
            _ if self.metered() => 3,
            _ => 2,
        }
    }
}

/// The interface the OS should route through by default. The user's choice
/// while it's connected; otherwise, when a metered network (a phone) and an
/// unmetered one are both up, the best unmetered one: wired before Wi-Fi.
/// `None` leaves the OS to its own choice.
fn pick_default(links: &[LinkInfo], chosen: Option<u32>) -> Option<u32> {
    let present = || links.iter().filter(|l| l.present);
    if let Some(chosen) = chosen {
        if present().any(|l| l.if_index == chosen) {
            return Some(chosen);
        }
    }
    if !present().any(LinkInfo::metered) {
        return None;
    }
    let rank = |l: &LinkInfo| match l.kind {
        "ethernet" => 0,
        "wifi" => 1,
        _ => 2,
    };
    present()
        .filter(|l| !l.metered())
        .min_by_key(|l| (rank(l), l.if_index))
        .map(|l| l.if_index)
}

fn kind_of(itf: &netdev::Interface) -> &'static str {
    #[cfg(target_os = "linux")]
    if let Some(kind) = sys::adapter_kind(&itf.name) {
        return kind;
    }
    #[cfg(target_os = "macos")]
    if let Some(kind) = sys::adapter_kind(&itf.name, itf.friendly_name.as_deref()) {
        return kind;
    }
    let desc = itf
        .description
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let tether = [
        "remote ndis",
        "rndis",
        "apple mobile device",
        "iphone",
        "android",
        "usb ethernet/rndis",
    ];
    if tether.iter().any(|t| desc.contains(t)) {
        return "phone";
    }
    match itf.if_type {
        InterfaceType::Wireless80211 => "wifi",
        InterfaceType::Wwanpp | InterfaceType::Wwanpp2 | InterfaceType::Wman => "cellular",
        InterfaceType::Ethernet
        | InterfaceType::GigabitEthernet
        | InterfaceType::FastEthernetT
        | InterfaceType::FastEthernetFx => "ethernet",
        _ => "other",
    }
}

fn is_global_v6(ip: &Ipv6Addr) -> bool {
    // 2000::/3 is the global unicast range.
    (ip.segments()[0] & 0xe000) == 0x2000
}

fn is_connected(itf: &netdev::Interface) -> bool {
    match itf.oper_state {
        OperState::Up => true,
        // Linux drivers that never report a state, such as RNDIS for Android
        // tethering, stay "unknown"; the running flag still follows the carrier.
        #[cfg(target_os = "linux")]
        OperState::Unknown => itf.is_up() && itf.is_running(),
        _ => false,
    }
}

/// Interfaces that could reach the internet (up, addressed, with a
/// gateway), plus disconnected hardware adapters so they can be shown as
/// "not connected".
fn discover() -> Vec<(String, LinkInfo)> {
    netdev::get_interfaces()
        .into_iter()
        .filter(|i| !i.is_loopback() && !engine::is_tun(i))
        // macOS lists VPN tunnels (Tailscale's among them), AirDrop and
        // bridges next to its ports, with gateways of their own; only the
        // ports are networks.
        .filter(|i| !cfg!(target_os = "macos") || sys::has_connector(i.index))
        .filter_map(|i| {
            let (gw_v4, gw_v6) = match &i.gateway {
                Some(gw) => (
                    gw.ipv4.iter().find(|g| !g.is_unspecified()).copied(),
                    gw.ipv6.iter().find(|g| !g.is_unspecified()).copied(),
                ),
                None => (None, None),
            };
            let ipv4 = i
                .ipv4
                .iter()
                .map(|n| n.addr())
                .find(|a| !a.is_link_local() && !a.is_unspecified())
                .filter(|_| gw_v4.is_some());
            let ipv6 = i
                .ipv6
                .iter()
                .map(|n| n.addr())
                .find(is_global_v6)
                .filter(|_| gw_v6.is_some());
            let present = is_connected(&i) && (ipv4.is_some() || ipv6.is_some());
            let kind = kind_of(&i);
            let description = i.description.clone();
            #[cfg(target_os = "linux")]
            let description = description.or_else(|| sys::adapter_description(i.index));
            let description = description.unwrap_or_default();
            // Virtual adapters (Hyper-V, VPNs, veth pairs) have no connector,
            // and Bluetooth PAN isn't worth a share.
            let hardware = kind != "other"
                && !description.to_lowercase().contains("bluetooth")
                && sys::has_connector(i.index);
            if !present && !hardware {
                return None;
            }
            let info = LinkInfo {
                if_index: i.index,
                name: i.friendly_name.clone().unwrap_or_else(|| i.name.clone()),
                description,
                kind,
                ipv4,
                ipv6,
                gateway: gw_v4
                    .map(|g| g.to_string())
                    .or(gw_v6.map(|g| g.to_string())),
                speed_bps: i.receive_speed.or(i.transmit_speed),
                present,
            };
            Some((i.name, info))
        })
        .collect()
}

#[derive(Default)]
pub struct Links {
    list: RwLock<Vec<Arc<Link>>>,
    /// Interface index of the network Windows routes through by default
    /// (0 when unknown).
    default_if: AtomicU32,
}

impl Links {
    pub fn all(&self) -> Vec<Arc<Link>> {
        self.list.read().unwrap().clone()
    }

    pub fn present(&self) -> Vec<Arc<Link>> {
        self.all()
            .into_iter()
            .filter(|l| l.info.read().unwrap().present)
            .collect()
    }

    /// The network Windows itself would use, as last seen without NetAgg
    /// in the way.
    pub fn default_link(&self) -> Option<Arc<Link>> {
        let index = self.default_if.load(Ordering::Relaxed);
        self.all()
            .into_iter()
            .find(|l| l.info.read().unwrap().if_index == index)
    }

    /// The interface the OS should route through by default (see
    /// `pick_default`), given the link the user chose, if any.
    pub fn preferred_default(&self, chosen: Option<&str>) -> Option<u32> {
        let chosen = chosen
            .and_then(|id| self.get(id))
            .map(|l| l.info().if_index);
        let infos: Vec<LinkInfo> = self.all().iter().map(|l| l.info()).collect();
        pick_default(&infos, chosen)
    }

    pub fn get(&self, id: &str) -> Option<Arc<Link>> {
        self.all().into_iter().find(|l| l.id == id)
    }

    /// Re-reads the adapters. Links keep their identity, and their counters,
    /// across address changes, unplugging, and plugging back in.
    /// Returns true when something changed.
    pub fn refresh(&self) -> bool {
        let found = discover();
        // While NetAgg is on, the default is its own adapter, which isn't in
        // the list; the last real default is kept then.
        if let Ok(default) = netdev::get_default_interface() {
            if found.iter().any(|(_, info)| info.if_index == default.index) {
                self.default_if.store(default.index, Ordering::Relaxed);
            }
        }
        let mut list = self.list.write().unwrap();
        let mut changed = false;

        for link in list.iter() {
            let mut info = link.info.write().unwrap();
            match found.iter().find(|(id, _)| *id == link.id) {
                Some((_, fresh)) => {
                    let addr_changed = info.ipv4 != fresh.ipv4
                        || info.ipv6 != fresh.ipv6
                        || info.present != fresh.present;
                    if addr_changed {
                        // A new address deserves a fresh health verdict.
                        *link.health.lock().unwrap() = Health::unknown();
                        changed = true;
                    }
                    *info = fresh.clone();
                }
                None => {
                    if info.present {
                        info.present = false;
                        changed = true;
                    }
                }
            }
        }

        for (id, info) in found {
            if !list.iter().any(|l| l.id == id) {
                list.push(Arc::new(Link::new(id, info)));
                changed = true;
            }
        }
        changed
    }

    /// Called once a second: turns byte counters into rates.
    pub fn tick(&self, now: Instant) {
        for link in self.all() {
            let busy = link.count_busy(now);
            link.stats.busy.store(busy, Ordering::Relaxed);
            let rx = link.stats.rx.load(Ordering::Relaxed);
            let tx = link.stats.tx.load(Ordering::Relaxed);
            let mut r = link.rates.lock().unwrap();
            r.rx_rate = rx.saturating_sub(r.last_rx);
            r.tx_rate = tx.saturating_sub(r.last_tx);
            r.last_rx = rx;
            r.last_tx = tx;
            let sample = (r.rx_rate, r.tx_rate);
            r.history.push_back(sample);
            while r.history.len() > HISTORY_LEN {
                r.history.pop_front();
            }
            // Decays to about half in 25 s, so capacity estimates follow
            // a network that has slowed down.
            let peak = link.stats.peak_rx.load(Ordering::Relaxed);
            let decayed = peak - peak / 36;
            link.stats
                .peak_rx
                .store(decayed.max(r.rx_rate), Ordering::Relaxed);
            // Half in an hour, at one tick a second.
            let capacity = link.stats.capacity.load(Ordering::Relaxed);
            let decayed = capacity - capacity / 5_194;
            link.stats
                .capacity
                .store(decayed.max(r.rx_rate), Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub fn from_list(list: Vec<Arc<Link>>) -> Links {
        Links {
            list: RwLock::new(list),
            default_if: AtomicU32::new(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(if_index: u32, kind: &'static str, name: &str, present: bool) -> LinkInfo {
        LinkInfo {
            if_index,
            name: name.into(),
            kind,
            present,
            ..test_info()
        }
    }

    /// Prints the first hops of each network's path. Run with
    /// `cargo test live_hops -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_hops() {
        let links = Links::default();
        links.refresh();
        for link in links.present() {
            let info = link.info();
            let Some(src) = info.ipv4 else { continue };
            let hops: Vec<String> = (1..=3)
                .map(|ttl| {
                    sys::icmp_hop(src, std::net::Ipv4Addr::new(1, 1, 1, 1), ttl, 1000)
                        .map_or("-".into(), |h| h.to_string())
                })
                .collect();
            println!(
                "{} (gw {:?}): {}",
                info.name,
                info.gateway,
                hops.join(" > ")
            );
        }
    }

    #[test]
    fn a_phone_on_the_same_wifi_is_found() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        let others = [("wifi", "198.51.100.1")];
        // The phone's path, as measured on the user's PC.
        assert_eq!(
            shared_with(
                &[ip("198.51.100.1"), ip("10.0.8.40")],
                Some("10.0.0.2"),
                &others
            ),
            Some("wifi")
        );
        // On mobile data the second hop is the carrier's.
        assert_eq!(
            shared_with(
                &[ip("10.64.0.1"), ip("10.64.3.9")],
                Some("10.0.0.2"),
                &others
            ),
            None
        );
        // A link's own gateway doesn't count, nor do lost probes.
        assert_eq!(
            shared_with(&[ip("198.51.100.1")], Some("198.51.100.1"), &others),
            None
        );
        assert_eq!(shared_with(&[], Some("10.0.0.2"), &others), None);
    }

    #[test]
    fn a_ramping_link_keeps_its_known_capacity() {
        // Wi-Fi has shown 60 MB/s; its first flow of a new burst is busy
        // but has only reached 5 MB/s so far.
        let wifi = Link::new("wifi".into(), test_info());
        wifi.stats.capacity.store(60 << 20, Ordering::Relaxed);
        wifi.stats.peak_rx.store(5 << 20, Ordering::Relaxed);
        let _flow = wifi.meter();
        assert!(wifi.busy() > 0);
        assert_eq!(wifi.capacity_estimate(), 60 << 20);
        // A faster recent peak still counts in full.
        wifi.stats.peak_rx.store(80 << 20, Ordering::Relaxed);
        assert_eq!(wifi.capacity_estimate(), 80 << 20);
    }

    #[test]
    fn phones_get_a_name_that_says_so() {
        assert_eq!(
            net(1, "phone", "Ethernet 2", true).display_name(),
            "Phone (USB tethering)"
        );
        assert_eq!(
            net(1, "phone", "usb0", true).display_name(),
            "Phone (USB tethering)"
        );
        assert_eq!(
            net(1, "phone", "iPhone USB", true).display_name(),
            "iPhone USB"
        );
        assert_eq!(
            net(1, "ethernet", "Ethernet", true).display_name(),
            "Ethernet"
        );
    }

    #[test]
    fn default_avoids_the_phone_unless_chosen() {
        let wifi = net(17, "wifi", "Wi-Fi", true);
        let phone = net(16, "phone", "Ethernet", true);
        let wired = net(20, "ethernet", "Ethernet 3", true);
        // Automatic: the phone never wins while something else is up.
        assert_eq!(pick_default(&[phone.clone(), wifi.clone()], None), Some(17));
        assert_eq!(
            pick_default(&[phone.clone(), wifi.clone(), wired.clone()], None),
            Some(20)
        );
        // Nothing metered: Windows' own choice stands.
        assert_eq!(pick_default(&[wifi.clone(), wired.clone()], None), None);
        // Only the phone: nothing better to choose.
        assert_eq!(pick_default(std::slice::from_ref(&phone), None), None);
        // The user's choice wins, the phone included...
        assert_eq!(
            pick_default(&[phone.clone(), wifi.clone()], Some(16)),
            Some(16)
        );
        // ...while it's connected; then automatic again.
        let gone = net(16, "phone", "Ethernet", false);
        assert_eq!(pick_default(&[gone.clone(), wifi.clone()], Some(16)), None);
        assert_eq!(pick_default(&[gone, wifi, wired], Some(16)), None);
    }

    #[test]
    fn busy_counts_new_and_downloading_flows() {
        let link = Link::new("wifi".into(), test_info());
        let downloading = link.meter();
        let idle = link.meter();
        // Not ticked yet: both are new.
        assert_eq!(link.busy(), 2);

        let later = Instant::now() + FRESH;
        downloading.add_rx(BUSY_BYTES);
        idle.add_rx(100);
        link.stats
            .busy
            .store(link.count_busy(later), Ordering::Relaxed);
        assert_eq!(link.busy(), 1);

        drop(downloading);
        link.stats
            .busy
            .store(link.count_busy(later), Ordering::Relaxed);
        assert_eq!(link.busy(), 0);
        assert_eq!(link.meters.lock().unwrap().len(), 1);
    }

    fn test_info() -> LinkInfo {
        LinkInfo {
            if_index: 1,
            name: "Wi-Fi".into(),
            description: String::new(),
            kind: "wifi",
            ipv4: Some(Ipv4Addr::new(192, 168, 1, 2)),
            ipv6: None,
            gateway: None,
            speed_bps: None,
            present: true,
        }
    }

    /// Prints what discovery sees on this machine. Run with
    /// `cargo test discovery -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn discovery_on_this_machine() {
        for (id, info) in discover() {
            println!("{id}\n  {info:?}");
        }
    }

    #[test]
    fn two_failures_take_a_link_down_and_success_restores_it() {
        let link = Link::new(
            "x".into(),
            LinkInfo {
                if_index: 1,
                name: "x".into(),
                description: String::new(),
                kind: "wifi",
                ipv4: Some(Ipv4Addr::new(192, 168, 1, 2)),
                ipv6: None,
                gateway: None,
                speed_bps: None,
                present: true,
            },
        );
        link.record_failure();
        assert!(link.usable());
        link.record_failure();
        assert!(!link.usable());
        link.record_success(Some(20));
        assert!(link.usable());
    }
}
