use std::fs;
use std::io;
use std::path::PathBuf;

const CONF: &str = "/proc/sys/net/ipv4/conf";
const STRICT: &str = "1";
const LOOSE: &str = "2";

/// Strict reverse-path filtering drops a reply that comes in on a link
/// whenever the route back to its sender goes elsewhere, and while
/// NetAgg is on that route always goes into the adapter. Loose mode only
/// asks for some route back. Strict settings (per interface, `all`, and
/// `default` for links that appear later) are loosened until this is dropped.
pub struct LooseRpFilter {
    loosened: Vec<PathBuf>,
}

impl LooseRpFilter {
    pub fn apply() -> io::Result<LooseRpFilter> {
        // Built first, so an error halfway restores what was already changed.
        let mut guard = LooseRpFilter {
            loosened: Vec::new(),
        };
        for entry in fs::read_dir(CONF)? {
            let path = entry?.path().join("rp_filter");
            if fs::read_to_string(&path).is_ok_and(|v| v.trim() == STRICT) {
                fs::write(&path, LOOSE)?;
                guard.loosened.push(path);
            }
        }
        if !guard.loosened.is_empty() {
            log::info!(
                "loosened reverse-path filtering on {} entries",
                guard.loosened.len()
            );
        }
        Ok(guard)
    }
}

/// Whether `interface` answers ARP for addresses that aren't its own, or
/// names the wrong one in its requests (the kernel's defaults). With two
/// adapters on one subnet the router can then send one adapter's replies
/// to the other, where a socket pinned to the first never sees them.
pub fn arp_flux_prone(interface: &str) -> bool {
    // The kernel uses the higher of `all` and the interface's own value.
    let value = |name: &str| {
        ["all", interface]
            .iter()
            .filter_map(|conf| fs::read_to_string(format!("{CONF}/{conf}/{name}")).ok())
            .filter_map(|v| v.trim().parse::<u8>().ok())
            .max()
            .unwrap_or(0)
    };
    value("arp_ignore") < 1 || value("arp_announce") < 2
}

impl Drop for LooseRpFilter {
    fn drop(&mut self) {
        for path in &self.loosened {
            // A link that has gone since took its setting with it.
            let _ = fs::write(path, STRICT);
        }
    }
}
