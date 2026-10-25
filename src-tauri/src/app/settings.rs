use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::net::balancer::{LinkPolicy, Policy};
use crate::net::dns::{DnsMode, Upstream};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LinkPrefs {
    pub enabled: bool,
    /// Relative share of new connections, 1 to 10.
    pub weight: u32,
    pub label: Option<String>,
    /// The link's long-term capacity in bytes/s (`LinkStats::capacity`),
    /// kept so a restart doesn't start the balancer blind.
    pub capacity: u64,
}

impl Default for LinkPrefs {
    fn default() -> Self {
        LinkPrefs {
            enabled: true,
            weight: 5,
            label: None,
            capacity: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// NetAgg was on when it last ran, so it turns back on at start.
    pub on: bool,
    /// Keyed by the adapter's GUID, which survives renames and reboots.
    pub links: HashMap<String, LinkPrefs>,
    /// Keep every connection to a site on the network it first used.
    pub sticky: bool,
    /// Scale each network's share by the throughput it has actually delivered.
    pub adaptive: bool,
    pub capture_ipv6: bool,
    /// Auto: each network's own DNS servers. Manual: `dns`.
    pub dns_mode: DnsMode,
    /// The servers for manual DNS, as typed (see `Upstream::parse`).
    pub dns: Vec<String>,
    /// With manual DNS, answer every app's queries with `dns` and block
    /// any that would go around NetAgg.
    pub enforce_dns: bool,
    /// Look for a new version when NetAgg starts. Off by default: it's the
    /// only thing NetAgg would contact on its own, and it never installs
    /// anything without the user's say-so.
    pub check_updates: bool,
    /// Leave networks that aren't there (unplugged, not connected) out of the
    /// popup's list.
    pub hide_unavailable: bool,
    /// The network the OS should route through by default (a link id), or
    /// automatic: never a phone while another network is up.
    pub default_link: Option<String>,
    /// The user wants NetAgg started at sign-in. Kept here as well as in
    /// the task itself: a manual upgrade runs the old uninstaller, which
    /// removes the task, and this puts it back.
    pub start_on_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            on: false,
            links: HashMap::new(),
            sticky: false,
            adaptive: true,
            capture_ipv6: true,
            // No servers picked for the user: Auto uses what each network
            // hands out, and Manual waits for the user's own choice.
            dns_mode: DnsMode::Auto,
            dns: Vec::new(),
            enforce_dns: true,
            check_updates: false,
            hide_unavailable: false,
            default_link: None,
            start_on_login: false,
        }
    }
}

impl Settings {
    pub fn link(&self, id: &str) -> LinkPrefs {
        self.links.get(id).cloned().unwrap_or_default()
    }

    /// What the balancer needs to know.
    pub fn policy(&self) -> Policy {
        Policy {
            links: self
                .links
                .iter()
                .map(|(id, p)| {
                    let policy = LinkPolicy {
                        enabled: p.enabled,
                        weight: p.weight.clamp(1, 10),
                    };
                    (id.clone(), policy)
                })
                .collect(),
            sticky: self.sticky,
            adaptive: self.adaptive,
            block_ipv6: !self.capture_ipv6,
        }
    }

    /// The manual servers, checked.
    pub fn upstreams(&self) -> Result<Vec<Upstream>, String> {
        self.dns.iter().map(|s| Upstream::parse(s)).collect()
    }

    /// Defaults when there's no file yet; anything else that goes wrong is
    /// logged, since the defaults then quietly replace the user's choices.
    pub fn load(path: &PathBuf) -> Settings {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
            Err(err) => {
                log::warn!("couldn't read {}: {err}", path.display());
                return Settings::default();
            }
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            log::warn!("couldn't parse {}: {err}", path.display());
            Settings::default()
        })
    }

    pub fn save(&self, path: &PathBuf) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(json) = serde_json::to_vec_pretty(self) else {
            return;
        };
        // Write then rename so a crash never leaves half a file.
        let tmp = path.with_extension("json.tmp");
        if let Err(err) = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, path)) {
            log::warn!("couldn't save {}: {err}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_checks_stay_off_unless_chosen() {
        assert!(!Settings::default().check_updates);
        // A settings file from 1.1.0 has no such field.
        let old: Settings = serde_json::from_str(r#"{"on":false,"sticky":true}"#).unwrap();
        assert!(!old.check_updates && old.sticky && !old.on);
        let chosen: Settings = serde_json::from_str(r#"{"checkUpdates":true}"#).unwrap();
        assert!(chosen.check_updates);
    }

    #[test]
    fn dns_is_auto_unless_chosen() {
        let fresh = Settings::default();
        assert_eq!(fresh.dns_mode, DnsMode::Auto);
        assert!(fresh.dns.is_empty() && fresh.enforce_dns);
        // 1.2.0 saved its fixed servers as plain addresses; they stay as the
        // manual list, but nothing switches to it.
        let old: Settings = serde_json::from_str(r#"{"dns":["8.8.8.8","1.1.1.1"]}"#).unwrap();
        assert_eq!(old.dns_mode, DnsMode::Auto);
        assert_eq!(old.upstreams().unwrap().len(), 2);
        let manual: Settings = serde_json::from_str(
            r#"{"dnsMode":"manual","dns":["tls://1.1.1.1"],"enforceDns":false}"#,
        )
        .unwrap();
        assert_eq!(manual.dns_mode, DnsMode::Manual);
        assert!(!manual.enforce_dns);
    }

    #[test]
    fn unavailable_networks_show_unless_hidden() {
        let old: Settings = serde_json::from_str(r#"{"on":false}"#).unwrap();
        assert!(!old.hide_unavailable);
        let chosen: Settings = serde_json::from_str(r#"{"hideUnavailable":true}"#).unwrap();
        assert!(chosen.hide_unavailable);
    }
}
