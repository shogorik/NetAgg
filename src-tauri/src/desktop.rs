//! The desktop app: the tray icon and its popup (Tauri), which ask the
//! engine for everything through a `Backend`.

use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, RunEvent, WindowEvent};

use crate::app::backend::Backend;
use crate::app::control::{OptionChange, Request};
use crate::app::{autostart, commands, logfile, popup, ticker, tray};
use crate::sys;

pub fn run() {
    let has_arg = |arg: &str| std::env::args().any(|a| a == arg);
    if let Some(code) = crate::run_mode() {
        std::process::exit(code);
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
        .plugin(tauri_plugin_opener::init())
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
            commands::webui_state,
            commands::set_webui,
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
            // Windows and Linux: the service runs the engine, and this process
            // never has administrator rights.
            #[cfg(not(target_os = "macos"))]
            let backend = Backend::Service;
            #[cfg(target_os = "macos")]
            let backend = {
                if elevated {
                    crate::engine::clean_up_stale();
                }
                let config_dir = app.path().app_config_dir()?;
                crate::app::state::import_from_multipath(&config_dir);
                let state = crate::app::state::AppState::new(config_dir);
                state.apply_default_route();
                Backend::Local(std::sync::Arc::new(state))
            };
            app.manage(backend);
            tray::create(app.handle())?;
            ticker::spawn(app.handle().clone());
            #[cfg(target_os = "linux")]
            commands::restart_when_replaced(app.handle().clone());
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
                // kept, so the next start turns on again. Not when restarting
                // into an upgraded copy: that one carries on as things are.
                let backend = app.state::<Backend>();
                let quit = Request::SetEnabled {
                    on: false,
                    remember: false,
                };
                if commands::restarting() {
                    log::info!("restarting: NetAgg stays as it is");
                } else if let Err(err) = tauri::async_runtime::block_on(backend.call(quit)) {
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
