//! Once a second, in the tray app: run the bookkeeping where the engine runs
//! in this process (`Ticks`), and tell the popup and the tray.

use std::time::Duration;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use super::backend::Backend;
use super::ticks::Ticks;
use super::{popup, status, tray};
use crate::net::health;

/// Shows a status in the tray and, if it's open, the popup.
pub fn show(app: &AppHandle, status: &Value) {
    let on = status["on"] == true;
    if let Err(err) = tray::set_active(app, on, &status::tooltip(status)) {
        log::warn!("could not update the tray icon: {err}");
    }
    if popup::is_visible(app) {
        let _ = app.emit_to(popup::LABEL, "status", status);
    }
}

pub fn spawn(app: AppHandle) {
    let local = app.state::<Backend>().local().cloned();
    if let Some(state) = &local {
        tauri::async_runtime::spawn(health::run(state.links.clone(), state.health_stop()));
    }

    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        let mut ticks = local.as_deref().map(Ticks::new);
        loop {
            interval.tick().await;
            if let (Some(ticks), Some(state)) = (&mut ticks, &local) {
                ticks.tick(state).await;
            }
            let status = app.state::<Backend>().status().await;
            show(&app, &status);
        }
    });
}
