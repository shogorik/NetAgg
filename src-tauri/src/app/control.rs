//! What the popup may ask of the engine, wherever the engine runs: in the
//! app itself (Linux, macOS), or in the NetAgg service (Windows), over its
//! pipe. Every request is checked here, on the engine's side, whoever sent
//! it. Each change replies with the new status.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::settings::LinkPrefs;
use super::state::AppState;
use super::status;
use crate::net::dns::{DnsMode, Upstream};
use crate::sys;

/// The reply to a change only an administrator may make, from a caller
/// that isn't one. The request checked out otherwise, so it can be sent
/// again from an elevated process.
pub const NEEDS_ADMIN: &str = "This change needs administrator approval.";
/// The reply to turning on in a process without the rights to (Linux,
/// macOS): the app relaunches itself with them.
pub const NEEDS_RIGHTS: &str = "NetAgg needs administrator rights to turn on.";

const MAX_LABEL: usize = 40;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Request {
    Status,
    Connections,
    /// `remember: false` is quitting: "on" is kept for the next start.
    SetEnabled {
        on: bool,
        remember: bool,
    },
    /// Turns on again if NetAgg was on when the app last quit.
    Resume,
    SetLinkEnabled {
        id: String,
        enabled: bool,
    },
    SetLinkWeight {
        id: String,
        weight: u32,
    },
    /// An empty name goes back to the automatic one.
    SetLinkLabel {
        id: String,
        label: String,
    },
    /// The network the OS routes through by default; `None` is automatic.
    SetDefaultLink {
        id: Option<String>,
    },
    SetOption {
        change: OptionChange,
    },
    /// Lets Windows keep Wi-Fi up next to a wired-looking network (see
    /// `Status::drops_wifi`). Windows applies it at the next sign-in.
    AllowSimultaneousConnections,
}

impl Request {
    /// DNS and the leak block decide where every lookup goes, and the
    /// Wi-Fi policy is a Windows setting: changing them takes an
    /// administrator, as it would without NetAgg.
    pub fn needs_admin(&self) -> bool {
        match self {
            Request::SetOption { change } => change.touches_dns(),
            Request::AllowSimultaneousConnections => true,
            _ => false,
        }
    }
}

/// The settings a change can carry; fields left out stay as they are.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OptionChange {
    pub adaptive: Option<bool>,
    pub sticky: Option<bool>,
    pub capture_ipv6: Option<bool>,
    pub dns_mode: Option<DnsMode>,
    pub dns: Option<Vec<String>>,
    pub enforce_dns: Option<bool>,
    pub start_on_login: Option<bool>,
    pub check_updates: Option<bool>,
    pub hide_unavailable: Option<bool>,
}

impl OptionChange {
    fn touches_dns(&self) -> bool {
        self.dns_mode.is_some() || self.dns.is_some() || self.enforce_dns.is_some()
    }
}

/// Who sent a request.
#[derive(Clone, Copy, Debug)]
pub struct Caller {
    /// Runs with administrator rights.
    pub elevated: bool,
}

pub async fn handle(state: &AppState, request: Request, caller: Caller) -> Result<Value, String> {
    match request {
        Request::Status => {}
        Request::Connections => return json(status::connections(state)),
        Request::SetEnabled { on, remember } => switch(state, on, remember).await?,
        Request::Resume => {
            if state.settings().on && !state.is_on() {
                switch(state, true, true).await?;
            }
        }
        Request::SetLinkEnabled { id, enabled } => {
            update_link(state, &id, |p| p.enabled = enabled)?;
            if !enabled {
                // Switching a network off moves its traffic too: apps
                // reconnect, and their new connections go elsewhere.
                if let Some(flows) = state.flows.read().unwrap().as_ref() {
                    flows.abort_on_link(&id, std::time::Duration::ZERO);
                }
            }
        }
        Request::SetLinkWeight { id, weight } => {
            update_link(state, &id, |p| p.weight = weight.clamp(1, 10))?
        }
        Request::SetLinkLabel { id, label } => {
            let label = label.trim();
            if label.chars().count() > MAX_LABEL {
                return Err(format!("keep the name under {MAX_LABEL} characters"));
            }
            let label = (!label.is_empty()).then(|| label.to_owned());
            update_link(state, &id, |p| p.label = label)?
        }
        Request::SetDefaultLink { id } => {
            if let Some(id) = &id {
                if state.links.get(id).is_none() {
                    return Err(format!("no network {id}"));
                }
            }
            state.update_settings(|s| s.default_link = id);
            state.apply_default_route();
        }
        Request::SetOption { change } => set_option(state, change, caller).await?,
        Request::AllowSimultaneousConnections => {
            if !caller.elevated {
                return Err(NEEDS_ADMIN.into());
            }
            #[cfg(windows)]
            tokio::task::spawn_blocking(sys::allow_simultaneous_connections)
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("Couldn't change the Windows setting: {e}"))?;
        }
    }
    json(status::build(state))
}

fn json(value: impl Serialize) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|e| e.to_string())
}

/// Turns NetAgg on or off.
async fn switch(state: &AppState, on: bool, remember: bool) -> Result<(), String> {
    if !on {
        state.turn_off(remember).await;
        return Ok(());
    }
    if let Err(err) = install_secure() {
        // Also shown when it was the start at boot that tried.
        *state.error.lock().unwrap() = Some(err.clone());
        return Err(err);
    }
    if !sys::is_elevated() && !sys::elevates_in_place() {
        return Err(NEEDS_RIGHTS.into());
    }
    state.turn_on().await
}

/// Refuses to run with administrator rights from a folder, or with
/// settings, that other programs could change (Windows, Linux). Debug builds run
/// from the source tree, so they only warn.
pub fn install_secure() -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    if let Err(err) = sys::check_install_secure() {
        if cfg!(debug_assertions) {
            log::warn!("{err}");
        } else {
            return Err(err.to_string());
        }
    }
    Ok(())
}

/// Changes the preferences of one network the popup listed.
fn update_link(
    state: &AppState,
    id: &str,
    change: impl FnOnce(&mut LinkPrefs),
) -> Result<(), String> {
    if state.links.get(id).is_none() {
        return Err(format!("no network {id}"));
    }
    state.update_settings(|s| change(s.links.entry(id.to_owned()).or_default()));
    Ok(())
}

/// Checks DNS servers typed by the user and keeps them as typed (trimmed);
/// empty entries are skipped.
fn parse_dns(servers: &[String]) -> Result<Vec<String>, String> {
    servers
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| Upstream::parse(s).map(|_| s.to_owned()))
        .collect()
}

/// Starting at sign-in is registered by the app, which runs as the user;
/// here it's only remembered.
async fn set_option(state: &AppState, change: OptionChange, caller: Caller) -> Result<(), String> {
    let dns = change.dns.as_deref().map(parse_dns).transpose()?;
    let before = state.settings();
    let manual_after = change.dns_mode.unwrap_or(before.dns_mode) == DnsMode::Manual;
    if manual_after && dns.as_ref().unwrap_or(&before.dns).is_empty() {
        return Err("Add your DNS servers first, then switch to them.".into());
    }
    if change.touches_dns() && !caller.elevated {
        return Err(NEEDS_ADMIN.into());
    }
    state.update_settings(|s| {
        if let Some(adaptive) = change.adaptive {
            s.adaptive = adaptive;
        }
        if let Some(sticky) = change.sticky {
            s.sticky = sticky;
        }
        if let Some(capture_ipv6) = change.capture_ipv6 {
            s.capture_ipv6 = capture_ipv6;
        }
        if let Some(dns_mode) = change.dns_mode {
            s.dns_mode = dns_mode;
        }
        if let Some(dns) = dns {
            s.dns = dns;
        }
        if let Some(enforce_dns) = change.enforce_dns {
            s.enforce_dns = enforce_dns;
        }
        if let Some(start_on_login) = change.start_on_login {
            s.start_on_login = start_on_login;
        }
        if let Some(check_updates) = change.check_updates {
            s.check_updates = check_updates;
        }
        if let Some(hide_unavailable) = change.hide_unavailable {
            s.hide_unavailable = hide_unavailable;
        }
    });
    let after = state.settings();
    // The resolver and the DNS guard are set up as the engine starts.
    if (before.dns_mode, &before.dns, before.enforce_dns)
        != (after.dns_mode, &after.dns, after.enforce_dns)
    {
        state.restart().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_servers_are_checked() {
        let typed = vec![
            "1.1.1.1".into(),
            " 2606:4700::1111 ".into(),
            "".into(),
            "tls://dns.quad9.net#9.9.9.9".into(),
        ];
        assert_eq!(
            parse_dns(&typed).unwrap(),
            ["1.1.1.1", "2606:4700::1111", "tls://dns.quad9.net#9.9.9.9"]
        );
        assert!(parse_dns(&["dns.google".into()]).is_err());
        assert!(parse_dns(&["https://dns.google/dns-query".into()]).is_err());
        let change: OptionChange = serde_json::from_str(r#"{"dnsMode":"manual"}"#).unwrap();
        assert_eq!(change.dns_mode, Some(DnsMode::Manual));
    }

    #[test]
    fn option_changes_are_partial() {
        let change: OptionChange = serde_json::from_str(r#"{"captureIpv6":false}"#).unwrap();
        assert_eq!(change.capture_ipv6, Some(false));
        assert!(change.adaptive.is_none() && change.dns.is_none());
        let change: OptionChange = serde_json::from_str(r#"{"checkUpdates":true}"#).unwrap();
        assert_eq!(change.check_updates, Some(true));
        let change: OptionChange = serde_json::from_str(r#"{"hideUnavailable":true}"#).unwrap();
        assert_eq!(change.hide_unavailable, Some(true));
    }

    fn option(json: &str) -> Request {
        Request::SetOption {
            change: serde_json::from_str(json).unwrap(),
        }
    }

    #[test]
    fn dns_and_windows_settings_need_an_administrator() {
        assert!(option(r#"{"dnsMode":"auto"}"#).needs_admin());
        assert!(option(r#"{"dns":["1.1.1.1"]}"#).needs_admin());
        assert!(option(r#"{"enforceDns":false}"#).needs_admin());
        assert!(option(r#"{"sticky":true,"enforceDns":true}"#).needs_admin());
        assert!(Request::AllowSimultaneousConnections.needs_admin());
        assert!(!option(r#"{"sticky":true,"startOnLogin":true}"#).needs_admin());
        assert!(!Request::SetEnabled {
            on: true,
            remember: true
        }
        .needs_admin());
        assert!(!Request::SetDefaultLink { id: None }.needs_admin());
    }

    #[test]
    fn requests_travel_as_tagged_json() {
        let line = serde_json::to_string(&option(r#"{"dns":["9.9.9.9"]}"#)).unwrap();
        assert!(line.starts_with(r#"{"cmd":"setOption","change":{"#));
        let back: Request = serde_json::from_str(&line).unwrap();
        assert!(back.needs_admin());
        let back: Request =
            serde_json::from_str(r#"{"cmd":"setLinkWeight","id":"a","weight":3}"#).unwrap();
        assert!(matches!(back, Request::SetLinkWeight { weight: 3, .. }));
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"runThis"}"#).is_err());
    }
}
