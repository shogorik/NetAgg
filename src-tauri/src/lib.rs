mod app;
mod engine;
mod net;
mod sys;

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

use app::backend::Backend;
use app::control::{OptionChange, Request};
use app::{autostart, commands, headless, logfile, popup, ticker, tray};

pub fn run() {
    let has_arg = |arg: &str| std::env::args().any(|a| a == arg);
    if has_arg(headless::HEADLESS_ARG) {
        std::process::exit(headless::run());
    }
    #[cfg(windows)]
    if has_arg(app::service::SERVICE_ARG) {
        std::process::exit(app::service::run());
    }
    #[cfg(windows)]
    if has_arg(app::service::ADMIN_REQUEST_ARG) {
        let args: Vec<String> = std::env::args().collect();
        std::process::exit(app::service::run_admin_request(&args));
    }
    #[cfg(target_os = "macos")]
    if has_arg(engine::HELPER_ARG) {
        let args: Vec<String> = std::env::args().collect();
        std::process::exit(engine::run_helper(&args));
    }
    // Wayland doesn't let a window place itself, so the popup would land
    // wherever the compositor puts it; through XWayland it goes in the corner.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GDK_BACKEND").is_none() && std::env::var_os("DISPLAY").is_some() {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    let args: Vec<String> = std::env::args().collect();
    let (relaunched, just_relaunched) = commands::relaunch_info(&args);
    // Only from a relaunch just now: the updater carries old arguments over, and
    // the saved state, not an old "--turn-on", decides then.
    let turn_on = just_relaunched && has_arg(commands::TURN_ON_ARG);
    let at_sign_in = has_arg(autostart::AUTOSTART_ARG);
    // The elevated copy waits for the copy that launched it to exit.
    let wait = if relaunched {
        Duration::from_secs(10)
    } else {
        Duration::ZERO
    };
    let instance = match sys::Instance::acquire(wait) {
        Ok(Some(instance)) => Some(instance),
        Ok(None) => {
            let _ = sys::Instance::signal_running();
            return;
        }
        Err(err) => {
            eprintln!("single-instance check failed: {err}");
            None
        }
    };

    tauri::Builder::default()
        // Signed self-updates: the popup checks, the user confirms, the
        // plugin verifies the download's signature, then it's installed and
        // NetAgg restarts.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(popup::PopupState::default())
        .invoke_handler(tauri::generate_handler![
            popup::fit_popup,
            popup::hide_popup,
            commands::get_status,
            commands::set_enabled,
            commands::set_link_enabled,
            commands::set_link_weight,
            commands::set_link_label,
            commands::set_default_link,
            commands::allow_simultaneous_connections,
            commands::set_option,
            commands::list_connections,
        ])
        .setup(move |app| {
            // A menu bar app: no Dock icon, no app menu.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let elevated = sys::is_elevated();
            if let Ok(dir) = app.path().app_local_data_dir() {
                logfile::init(&dir);
            }
            log::info!(
                "NetAgg {} started from {:?}, elevated: {elevated}",
                env!("CARGO_PKG_VERSION"),
                std::env::current_exe().ok()
            );
            // Windows: the service runs the engine, and this process never
            // has administrator rights.
            #[cfg(windows)]
            let backend = Backend::Service;
            #[cfg(not(windows))]
            let backend = {
                if elevated {
                    engine::clean_up_stale();
                }
                let state = app::state::AppState::new(app.path().app_config_dir()?);
                state.apply_default_route();
                Backend::Local(std::sync::Arc::new(state))
            };
            app.manage(backend);
            tray::create(app.handle())?;
            ticker::spawn(app.handle().clone());
            // NetAgg comes back the way it was left. Without the rights to turn
            // on, it relaunches with them (pkexec), as the toggle does; a copy
            // relaunched for them just now that still lacks them doesn't try
            // again (see `commands::relaunch_info`).
            let can_start = elevated || sys::elevates_in_place() || !just_relaunched;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                restore_start_on_login(&handle).await;
                if can_start {
                    let request = match turn_on {
                        true => Request::SetEnabled {
                            on: true,
                            remember: true,
                        },
                        false => Request::Resume,
                    };
                    let _ = commands::ask(&handle, request).await;
                }
            });

            if let Some(instance) = &instance {
                let handle = app.handle().clone();
                instance.on_show_requested(move || popup::show(&handle));
            }
            app.manage(Mutex::new(instance));
            // At sign-in NetAgg starts quietly in the tray.
            if !at_sign_in {
                popup::show(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != popup::LABEL {
                return;
            }
            // The popup is only ever hidden; quitting goes through the tray menu.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                popup::hide(window.app_handle());
            }
        })
        .build(tauri::generate_context!())
        .expect("error while starting NetAgg")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                // Give the PC its own routing back before going; "on" is
                // kept, so the next start turns on again.
                let backend = app.state::<Backend>();
                let quit = Request::SetEnabled {
                    on: false,
                    remember: false,
                };
                if let Err(err) = tauri::async_runtime::block_on(backend.call(quit)) {
                    log::warn!("couldn't turn off: {err}");
                }
                if let Some(state) = backend.local() {
                    state.save_usage();
                }
                // A restart after an update starts the new copy before this
                // one is gone; it must not find this copy still running.
                if let Some(instance) = app.try_state::<Mutex<Option<sys::Instance>>>() {
                    instance.lock().unwrap().take();
                }
            }
        });
}

/// Keeps starting at sign-in and the user's wish in step: a registration
/// found is remembered, and a remembered one that's gone (Windows: versions
/// before the service registered a scheduled task, which the installer
/// removes) is registered again.
async fn restore_start_on_login(app: &AppHandle) {
    let Ok(status) = app.state::<Backend>().call(Request::Status).await else {
        return;
    };
    let wanted = status["settings"]["startOnLogin"] == true;
    let registered = autostart::is_enabled();
    if registered && !wanted {
        let change = OptionChange {
            start_on_login: Some(true),
            ..Default::default()
        };
        let _ = app
            .state::<Backend>()
            .call(Request::SetOption { change })
            .await;
    } else if wanted && !registered {
        match autostart::set_enabled(true) {
            Ok(()) => log::info!("registered starting at sign-in again"),
            Err(err) => log::warn!("couldn't register starting at sign-in again: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    fn json(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    /// The updater compares against the bundle's version, the popup shows
    /// Cargo's, and npm has its own: a release must bump all three.
    #[test]
    fn versions_agree() {
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(json(include_str!("../tauri.conf.json"))["version"], version);
        assert_eq!(json(include_str!("../../package.json"))["version"], version);
    }

    #[test]
    fn updates_come_signed_from_the_release_repo() {
        let conf = json(include_str!("../tauri.conf.json"));
        let updater = &conf["plugins"]["updater"];
        assert_eq!(
            updater["endpoints"],
            serde_json::json!([
                "https://github.com/shogorik/NetAgg/releases/latest/download/latest.json"
            ])
        );
        // A minisign public key (base64 of its text form), never a secret key.
        let pubkey = updater["pubkey"].as_str().unwrap();
        assert!(pubkey.starts_with("dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6"));
        // A signature only for an older release must not pass for a newer one.
        assert_eq!(updater["requireSignedVersion"], true);
        for risky in [
            "dangerousInsecureTransportProtocol",
            "dangerousAcceptInvalidCerts",
            "dangerousAcceptInvalidHostnames",
        ] {
            assert!(updater.get(risky).is_none(), "{risky}");
        }
        // Read the way the plugin reads it at start, so a misspelt key
        // can't quietly leave a check off.
        let parsed: tauri_plugin_updater::Config = serde_json::from_value(updater.clone()).unwrap();
        assert!(parsed.require_signed_version && !parsed.pubkey.is_empty());
        assert!(parsed.endpoints.iter().all(|url| url.scheme() == "https"));
        assert!(!parsed.dangerous_insecure_transport_protocol);
        assert!(
            !parsed.dangerous_accept_invalid_certs && !parsed.dangerous_accept_invalid_hostnames
        );
    }

    /// The popup may check, download (verified), install and restart;
    /// nothing broader such as `updater:default` or `process:allow-exit`.
    #[test]
    fn the_popup_gets_only_the_update_permissions_it_uses() {
        let caps = json(include_str!("../capabilities/default.json"));
        let mut granted: Vec<&str> = caps["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .filter(|p| p.starts_with("updater:") || p.starts_with("process:"))
            .collect();
        granted.sort();
        assert_eq!(
            granted,
            [
                "process:allow-restart",
                "updater:allow-check",
                "updater:allow-download",
                "updater:allow-install"
            ]
        );
    }

    /// Checks `release-assets/` the way the updater will: each bundle's
    /// signature against the public key in tauri.conf.json, and the version
    /// it was signed for against the one latest.json announces. A changed
    /// byte must fail. Run after `scripts/release.sh`:
    /// `cargo test -- --ignored release_assets`.
    #[test]
    #[ignore]
    fn release_assets_verify() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use minisign_verify::{PublicKey, Signature};

        let decode = |b64: &str| String::from_utf8(STANDARD.decode(b64.trim()).unwrap()).unwrap();
        let conf = json(include_str!("../tauri.conf.json"));
        let pubkey = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        let key = PublicKey::decode(&decode(pubkey)).unwrap();

        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../release-assets");
        let feed = json(&std::fs::read_to_string(assets.join("latest.json")).unwrap());
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(feed["version"], version);
        let base = format!("https://github.com/shogorik/NetAgg/releases/download/v{version}/");
        let platforms = feed["platforms"].as_object().unwrap();
        assert!(!platforms.is_empty());

        for (platform, entry) in platforms {
            let url = entry["url"].as_str().unwrap();
            let name = url
                .strip_prefix(&base)
                .unwrap_or_else(|| panic!("{platform}: {url}"));
            let signature =
                Signature::decode(&decode(entry["signature"].as_str().unwrap())).unwrap();
            let mut bytes = std::fs::read(assets.join(name)).unwrap();
            key.verify(&bytes, &signature, true)
                .unwrap_or_else(|e| panic!("{platform}: {e}"));
            assert!(signature
                .trusted_comment()
                .split('\t')
                .any(|field| field == format!("version:{version}")));

            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            assert!(key.verify(&bytes, &signature, true).is_err(), "{platform}");
        }
    }
}
