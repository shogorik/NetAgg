//! Commands the popup can invoke, passed on to the engine (see `control`).
//! Each change returns the new status, so the popup updates without
//! waiting for the next tick.

use serde_json::Value;
use tauri::{AppHandle, Manager};

use super::autostart;
use super::backend::Backend;
use super::control::{OptionChange, Request, NEEDS_RIGHTS};
use super::ticker;
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

/// Set when the tray app restarts into an upgraded copy of itself: the engine
/// stays as it is instead of being turned off on the way out.
pub static RESTARTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the tray app is on its way to a newer copy of itself: restarting on
/// its own, or by the updater after it installed the new package (Linux; on
/// Windows the updater exits without the app's exit event).
pub fn restarting() -> bool {
    #[cfg(target_os = "linux")]
    if std::env::current_exe().is_ok_and(|exe| exe.to_string_lossy().ends_with(" (deleted)")) {
        return true;
    }
    RESTARTING.load(std::sync::atomic::Ordering::SeqCst)
}

/// A package upgrade (apt, or the in-app update's dpkg) replaces the program
/// under the running tray app, which would go on running the old version;
/// Linux then names it "<path> (deleted)". The app restarts itself into the
/// new one, quietly (as at sign-in), and NetAgg stays on or off as it was.
#[cfg(target_os = "linux")]
pub fn restart_when_replaced(app: AppHandle) {
    use std::time::Duration;
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let Ok(exe) = std::env::current_exe() else {
                continue;
            };
            let new = sys::without_deleted(exe.clone());
            if new == exe || !new.is_file() {
                continue;
            }
            // dpkg unpacks first and configures after: let it finish.
            tokio::time::sleep(Duration::from_secs(5)).await;
            log::info!(
                "{} was upgraded: restarting into the new version",
                new.display()
            );
            // The new copy waits for this one's single-instance lock (`relaunch_info`).
            let started = std::process::Command::new(&new)
                .args([elevated_arg(), autostart::AUTOSTART_ARG.to_owned()])
                .spawn();
            match started {
                Ok(_) => {
                    RESTARTING.store(true, std::sync::atomic::Ordering::SeqCst);
                    app.exit(0);
                }
                Err(err) => log::warn!("couldn't restart into the new version: {err}"),
            }
            return;
        }
    });
}

/// Asks the engine, and shows a new status at once. Where the app runs the
/// engine itself without the rights to turn on (Linux), it restarts
/// elevated, through the pkexec prompt, and this copy exits. On Windows the
/// service has the rights; macOS asks for the password as it turns on.
pub async fn ask(app: &AppHandle, request: Request) -> Result<Value, String> {
    let reply = app.state::<Backend>().call(request).await;
    match &reply {
        Ok(status) => ticker::show(app, status),
        Err(err) if err == NEEDS_RIGHTS => {
            sys::relaunch_elevated(&format!("{} {TURN_ON_ARG}", elevated_arg()))
                .map_err(|e| e.to_string())?;
            app.exit(0);
        }
        Err(_) => {}
    }
    reply
}

/// Turns NetAgg on or off, and remembers it for the next start.
pub async fn switch(app: &AppHandle, on: bool) -> Result<Value, String> {
    ask(app, Request::SetEnabled { on, remember: true }).await
}

#[tauri::command]
pub async fn get_status(app: AppHandle) -> Value {
    app.state::<Backend>().status().await
}

#[tauri::command]
pub async fn set_enabled(app: AppHandle, enabled: bool) -> Result<Value, String> {
    switch(&app, enabled).await
}

#[tauri::command]
pub async fn set_link_enabled(app: AppHandle, id: String, enabled: bool) -> Result<Value, String> {
    ask(&app, Request::SetLinkEnabled { id, enabled }).await
}

#[tauri::command]
pub async fn set_link_label(app: AppHandle, id: String, label: String) -> Result<Value, String> {
    ask(&app, Request::SetLinkLabel { id, label }).await
}

#[tauri::command]
pub async fn allow_simultaneous_connections(app: AppHandle) -> Result<Value, String> {
    ask(&app, Request::AllowSimultaneousConnections).await
}

#[tauri::command]
pub async fn set_default_link(app: AppHandle, id: Option<String>) -> Result<Value, String> {
    ask(&app, Request::SetDefaultLink { id }).await
}

#[tauri::command]
pub async fn set_link_weight(app: AppHandle, id: String, weight: u32) -> Result<Value, String> {
    ask(&app, Request::SetLinkWeight { id, weight }).await
}

/// Starting at sign-in is registered here, as the user who signs in; the
/// engine only remembers the choice.
#[tauri::command]
pub async fn set_option(app: AppHandle, change: OptionChange) -> Result<Value, String> {
    if let Some(enabled) = change.start_on_login {
        tauri::async_runtime::spawn_blocking(move || autostart::set_enabled(enabled))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("Couldn't change starting at sign-in: {e}"))?;
    }
    ask(&app, Request::SetOption { change }).await
}

#[tauri::command]
pub async fn list_connections(app: AppHandle) -> Result<Value, String> {
    app.state::<Backend>().call(Request::Connections).await
}

/// The WebUI the Linux desktop package carries, switched off at first:
/// whether it runs and has a password. `None` where there's no WebUI.
#[tauri::command]
pub async fn webui_state() -> Option<Value> {
    #[cfg(all(feature = "webui", target_os = "linux"))]
    return tauri::async_runtime::spawn_blocking(|| {
        let (on, has_password) = super::webui::switch_state();
        serde_json::json!({ "on": on, "hasPassword": has_password })
    })
    .await
    .ok();
    #[cfg(not(all(feature = "webui", target_os = "linux")))]
    None
}

/// Turns the WebUI on (setting a new password first, if given) or off, as
/// root through polkit; the password goes on pkexec's standard input.
#[tauri::command]
pub async fn set_webui(on: bool, password: Option<String>) -> Result<Option<Value>, String> {
    #[cfg(all(feature = "webui", target_os = "linux"))]
    return switch_webui(on, password).await;
    #[cfg(not(all(feature = "webui", target_os = "linux")))]
    {
        let _ = (on, password);
        Err("There's no WebUI on this system.".to_owned())
    }
}

#[cfg(all(feature = "webui", target_os = "linux"))]
async fn switch_webui(on: bool, password: Option<String>) -> Result<Option<Value>, String> {
    let input =
        serde_json::to_string(&super::webui::Switch { on, password }).map_err(|e| e.to_string())?;
    let exe = sys::without_deleted(std::env::current_exe().map_err(|e| e.to_string())?);
    let code = tauri::async_runtime::spawn_blocking(move || {
        sys::run_elevated(&exe.to_string_lossy(), super::webui::SWITCH_ARG, &input)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|_| "The change needs administrator approval, which wasn't given.".to_owned())?;
    if code != 0 {
        return Err(format!(
            "The WebUI couldn't be switched {} (code {code}).",
            if on { "on" } else { "off" }
        ));
    }
    Ok(webui_state().await)
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
}
