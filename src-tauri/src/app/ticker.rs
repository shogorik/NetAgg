//! Once a second: turn byte counters into rates, count today's data, keep
//! the network list fresh (where the engine runs: `Ticks`), and tell the
//! popup and the tray.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use super::backend::Backend;
use super::state::AppState;
use super::{popup, status, tray};
use crate::net::{health, links::Links};
use crate::sys;

/// Adapters are re-read this often; reading them takes a few milliseconds.
const REFRESH_EVERY: u64 = 3;
const SAVE_USAGE_EVERY: u64 = 30;
const SHARED_LINE_EVERY: u64 = 60;

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

fn total_bytes(links: &Links) -> u64 {
    links
        .all()
        .iter()
        .map(|l| l.stats.rx.load(Ordering::Relaxed) + l.stats.tx.load(Ordering::Relaxed))
        .sum()
}

/// One second of bookkeeping, shared with the headless runner.
pub struct Ticks {
    n: u64,
    last_total: u64,
}

impl Ticks {
    pub fn new(state: &AppState) -> Ticks {
        Ticks {
            n: 0,
            last_total: total_bytes(&state.links),
        }
    }

    /// Rates, today's data, a fresh network list now and then, and cleaning
    /// up after an engine that stopped by itself.
    pub async fn tick(&mut self, state: &AppState) {
        self.n += 1;
        state.links.tick(Instant::now());

        let total = total_bytes(&state.links);
        state
            .usage
            .lock()
            .unwrap()
            .add(sys::local_date(), total.saturating_sub(self.last_total));
        self.last_total = total;
        if self.n.is_multiple_of(SAVE_USAGE_EVERY) {
            state.save_usage();
        }

        // Once a minute (and soon after start): which links share a line.
        if self.n % SHARED_LINE_EVERY == 5 {
            let links = state.links.clone();
            tokio::task::spawn_blocking(move || crate::net::links::check_shared_lines(&links));
        }

        if self.n.is_multiple_of(REFRESH_EVERY) {
            let links = state.links.clone();
            let _ = tokio::task::spawn_blocking(move || links.refresh()).await;
            state.restore_capacities();
            // Networks come and go; the default follows the choice.
            state.apply_default_route();
        }
        state.check_engine().await;
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
