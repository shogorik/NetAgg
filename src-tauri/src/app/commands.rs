//! Commands the popup can invoke. Each change returns the new status, so
//! the popup updates without waiting for the next tick.

use std::sync::atomic::Ordering;

use serde::Deserialize;
use tauri::{AppHandle, Manager, State};

use super::autostart;
use super::settings::LinkPrefs;
use super::state::AppState;
use super::status::{self, Connection, Status};
use super::ticker;
use crate::net::dns::{DnsMode, Upstream};
use crate::sys;

/// Passed to the elevated copy so it knows to wait for this one to exit.
pub const ELEVATED_ARG: &str = "--elevated";
/// A relaunch younger than this that still lacks administrator rights
/// means elevation failed. An older one is an argument the updater carried
/// over from the elevated copy it replaced (it restarts the new version with
/// the old one's arguments, without the rights).
const FRESH_RELAUNCH_SECS: u64 = 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `--elevated=<when>`, the time stamping it as this relaunch.
fn elevated_arg() -> String {
    format!("{ELEVATED_ARG}={}", now_secs())
}

/// From the command line: whether this copy was relaunched for the rights
/// (so it waits for the old copy to go), and whether that was just now.
pub fn relaunch_info(args: &[String]) -> (bool, bool) {
    args.iter()
        .find(|a| *a == ELEVATED_ARG || a.starts_with(&format!("{ELEVATED_ARG}=")))
        .map_or((false, false), |arg| {
            let fresh = arg
                .split_once('=')
                .and_then(|(_, at)| at.parse::<u64>().ok())
                .is_some_and(|at| now_secs().saturating_sub(at) < FRESH_RELAUNCH_SECS);
            (true, fresh)
        })
}
/// Passed along with `ELEVATED_ARG` when the user asked to turn on.
pub const TURN_ON_ARG: &str = "--turn-on";

/// Turns NetAgg on or off. Turning on without administrator rights
/// restarts NetAgg elevated, through the UAC (or pkexec) prompt, and this
/// copy exits. macOS asks for the password as it turns on instead.
pub async fn switch(app: &AppHandle, on: bool) -> Result<(), String> {
    let state = app.state::<AppState>();
    if on {
        if let Err(err) = install_secure() {
            // Also shown when it was the start at sign-in that tried.
            *state.error.lock().unwrap() = Some(err.clone());
            ticker::publish(app);
            return Err(err);
        }
    }
    let result = if !on {
        state.turn_off(true).await;
        Ok(())
    } else if sys::is_elevated() || sys::elevates_in_place() {
        state.turn_on().await
    } else {
        sys::relaunch_elevated(&format!("{} {TURN_ON_ARG}", elevated_arg()))
            .map_err(|e| e.to_string())?;
        app.exit(0);
        Ok(())
    };
    ticker::publish(app);
    result
}

/// Refuses to run with administrator rights from a folder other programs
/// could change (Windows). Debug builds run from the source tree, so they
/// only warn.
pub fn install_secure() -> Result<(), String> {
    #[cfg(windows)]
    if let Err(err) = sys::check_install_secure() {
        if cfg!(debug_assertions) {
            log::warn!("{err}");
        } else {
            return Err(err.to_string());
        }
    }
    Ok(())
}

fn current(app: &AppHandle) -> Status {
    ticker::publish(app);
    status::build(&app.state::<AppState>())
}

#[tauri::command]
pub fn get_status(state: State<AppState>) -> Status {
    status::build(&state)
}

#[tauri::command]
pub async fn set_enabled(app: AppHandle, enabled: bool) -> Result<Status, String> {
    switch(&app, enabled).await?;
    Ok(current(&app))
}

/// Changes the preferences of one network the popup listed.
fn update_link(
    app: &AppHandle,
    id: String,
    change: impl FnOnce(&mut LinkPrefs),
) -> Result<Status, String> {
    let state = app.state::<AppState>();
    if state.links.get(&id).is_none() {
        return Err(format!("no network {id}"));
    }
    state.update_settings(|s| change(s.links.entry(id).or_default()));
    Ok(current(app))
}

#[tauri::command]
pub fn set_link_enabled(app: AppHandle, id: String, enabled: bool) -> Result<Status, String> {
    let status = update_link(&app, id.clone(), |p| p.enabled = enabled)?;
    if !enabled {
        // Switching a network off moves its traffic too: apps reconnect,
        // and their new connections go elsewhere.
        if let Some(flows) = app.state::<AppState>().flows.read().unwrap().as_ref() {
            flows.abort_on_link(&id);
        }
    }
    Ok(status)
}

/// Names a network; an empty name goes back to the automatic one.
#[tauri::command]
pub fn set_link_label(app: AppHandle, id: String, label: String) -> Result<Status, String> {
    let label = label.trim();
    if label.chars().count() > MAX_LABEL {
        return Err(format!("keep the name under {MAX_LABEL} characters"));
    }
    let label = (!label.is_empty()).then(|| label.to_owned());
    update_link(&app, id, |p| p.label = label)
}

const MAX_LABEL: usize = 40;

/// Lets Windows keep Wi-Fi up next to a wired-looking network (see
/// `Status::drops_wifi`). Windows applies it at the next sign-in.
#[tauri::command]
pub async fn allow_simultaneous_connections(app: AppHandle) -> Result<Status, String> {
    #[cfg(windows)]
    tauri::async_runtime::spawn_blocking(sys::allow_simultaneous_connections)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("Couldn't change the Windows setting: {e}"))?;
    Ok(current(&app))
}

/// Chooses the network the OS routes through by default; `None` is automatic.
#[tauri::command]
pub fn set_default_link(app: AppHandle, id: Option<String>) -> Result<Status, String> {
    let state = app.state::<AppState>();
    if let Some(id) = &id {
        if state.links.get(id).is_none() {
            return Err(format!("no network {id}"));
        }
    }
    state.update_settings(|s| s.default_link = id);
    state.apply_default_route();
    Ok(current(&app))
}

#[tauri::command]
pub fn set_link_weight(app: AppHandle, id: String, weight: u32) -> Result<Status, String> {
    update_link(&app, id, |p| p.weight = weight.clamp(1, 10))
}

/// The settings a change can carry; fields left out stay as they are.
#[derive(Debug, Default, Deserialize)]
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

#[tauri::command]
pub async fn set_option(app: AppHandle, change: OptionChange) -> Result<Status, String> {
    let state = app.state::<AppState>();
    let dns = change.dns.as_deref().map(parse_dns).transpose()?;
    if let Some(enabled) = change.start_on_login {
        // The startup task runs NetAgg elevated without asking.
        if enabled {
            install_secure()?;
        }
        tauri::async_runtime::spawn_blocking(move || autostart::set_enabled(enabled))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("Couldn't change starting at sign-in: {e}"))?;
        state.start_on_login.store(enabled, Ordering::Relaxed);
        state.update_settings(|s| s.start_on_login = enabled);
    }

    let before = state.settings();
    let manual_after = change.dns_mode.unwrap_or(before.dns_mode) == DnsMode::Manual;
    if manual_after && dns.as_ref().unwrap_or(&before.dns).is_empty() {
        return Err("Add your DNS servers first, then switch to them.".into());
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
    Ok(current(&app))
}

#[tauri::command]
pub fn list_connections(state: State<AppState>) -> Vec<Connection> {
    status::connections(&state)
}

/// Called between a verified download and its install. On Windows the
/// updater starts the installer and exits the process at once, without
/// `RunEvent::Exit`, so the routes must be given back here; the saved "on"
/// stays, so the new version turns on again. Elsewhere the restart that
/// follows the install does this through `RunEvent::Exit`.
#[tauri::command]
pub async fn prepare_update_install(app: AppHandle) {
    if cfg!(windows) {
        let state = app.state::<AppState>();
        state.turn_off(false).await;
        state.save_usage();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_fresh_relaunch_counts_as_failed_elevation() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(relaunch_info(&args(&["netagg"])), (false, false));
        assert_eq!(
            relaunch_info(&args(&["netagg", &elevated_arg(), TURN_ON_ARG])),
            (true, true)
        );
        // Carried over by the updater from an elevated copy started long ago.
        let old = format!("{ELEVATED_ARG}={}", now_secs() - 3600);
        assert_eq!(relaunch_info(&args(&["netagg", &old])), (true, false));
        // 1.3.4 and older passed no time.
        assert_eq!(
            relaunch_info(&args(&["netagg", ELEVATED_ARG])),
            (true, false)
        );
    }

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
}
