//! Once a second where the engine runs (the app itself, the service or the
//! headless runner): turn byte counters into rates, count today's data and
//! keep the network list fresh.

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::state::AppState;
use crate::net::links::Links;
use crate::sys;

/// Adapters are re-read this often; reading them takes a few milliseconds.
const REFRESH_EVERY: u64 = 3;
const SAVE_USAGE_EVERY: u64 = 30;
const SHARED_LINE_EVERY: u64 = 60;

fn total_bytes(links: &Links) -> u64 {
    links
        .all()
        .iter()
        .map(|l| l.stats.rx.load(Ordering::Relaxed) + l.stats.tx.load(Ordering::Relaxed))
        .sum()
}

/// One second of bookkeeping.
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
