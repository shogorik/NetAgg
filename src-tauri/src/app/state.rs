//! What the commands, the tray and the ticker share: the networks, the
//! settings, and the engine while it runs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use super::settings::Settings;
use super::usage::Usage;
use crate::engine::{Engine, EngineConfig, Flows};
use crate::net::dns::{DnsMode, Resolver};
use crate::net::links::Links;
use crate::net::router::Router;

/// Long enough for the relays to wind down; the routes go first regardless.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The config folder of MultiPath, which NetAgg was forked from.
const MULTIPATH_DIR: &str = "dev.multipath.app";

/// A first run takes over the settings and usage a MultiPath install left
/// next to this config folder. Never overwrites, so it happens once. Only
/// for the user's own config folder: next to the service's, in ProgramData,
/// anyone could have left one.
pub fn import_from_multipath(config_dir: &std::path::Path) {
    let Some(old_dir) = config_dir.parent().map(|p| p.join(MULTIPATH_DIR)) else {
        return;
    };
    if config_dir.join("settings.json").exists() || !old_dir.join("settings.json").exists() {
        return;
    }
    let _ = std::fs::create_dir_all(config_dir);
    for file in ["settings.json", "usage.json"] {
        let from = old_dir.join(file);
        if from.exists() {
            match std::fs::copy(&from, config_dir.join(file)) {
                Ok(_) => log::info!("imported {} from MultiPath", file),
                Err(err) => log::warn!("couldn't import {file} from MultiPath: {err}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Imports this machine's real MultiPath settings into a temporary
    /// folder. Run with `cargo test real_import -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_import() {
        let appdata = std::path::PathBuf::from(std::env::var("APPDATA").unwrap());
        let root = std::env::temp_dir().join(format!("netagg-real-{}", std::process::id()));
        std::fs::create_dir_all(root.join(MULTIPATH_DIR)).unwrap();
        for file in ["settings.json", "usage.json"] {
            std::fs::copy(
                appdata.join(MULTIPATH_DIR).join(file),
                root.join(MULTIPATH_DIR).join(file),
            )
            .unwrap();
        }
        let new = root.join("dev.netagg");
        import_from_multipath(&new);
        println!(
            "{}",
            std::fs::read_to_string(new.join("settings.json")).unwrap()
        );
        let state = AppState::new(new.clone());
        println!("loaded dns mode: {:?}", state.settings().dns_mode);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn imports_multipath_settings_once() {
        let root = std::env::temp_dir().join(format!("netagg-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let old = root.join(MULTIPATH_DIR);
        let new = root.join("dev.netagg");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.json"), r#"{"dnsMode":"manual"}"#).unwrap();

        import_from_multipath(&new);
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            r#"{"dnsMode":"manual"}"#
        );
        assert!(!new.join("usage.json").exists());

        // Later changes in NetAgg are never overwritten.
        std::fs::write(new.join("settings.json"), "{}").unwrap();
        import_from_multipath(&new);
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            "{}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// Byte counts of each flow when the connections list was last read, to
/// turn them into rates.
#[derive(Default)]
pub struct FlowCounts {
    pub at: Option<Instant>,
    pub bytes: HashMap<u64, (u64, u64)>,
}

pub struct AppState {
    pub links: Arc<Links>,
    pub router: Arc<Router>,
    pub settings: Mutex<Settings>,
    settings_path: PathBuf,
    pub usage: Mutex<Usage>,
    usage_path: PathBuf,
    engine: tokio::sync::Mutex<Option<Engine>>,
    /// The running engine's flows, readable without waiting on the engine lock.
    pub flows: RwLock<Option<Arc<Flows>>>,
    /// Turning on or off is under way.
    pub busy: AtomicBool,
    /// Why NetAgg last failed to turn on, or stopped by itself.
    pub error: Mutex<Option<String>>,
    pub flow_counts: Mutex<FlowCounts>,
    /// The interface last made the default route (0: none), so it's only
    /// changed when the choice changes. Starts unknown, so the first call
    /// also undoes a choice an earlier run left.
    default_applied: AtomicU32,
    /// Keeps the health probes running for as long as the app does.
    health_stop: watch::Sender<bool>,
}

impl AppState {
    pub fn new(config_dir: PathBuf) -> AppState {
        let settings_path = config_dir.join("settings.json");
        let usage_path = config_dir.join("usage.json");
        let settings = Settings::load(&settings_path);
        log::info!(
            "settings: {} (exists: {}), dns {:?}",
            settings_path.display(),
            settings_path.exists(),
            settings.dns_mode
        );
        let links = Arc::new(Links::default());
        links.refresh();
        for link in links.all() {
            let saved = settings.link(&link.id).capacity;
            link.stats.capacity.store(saved, Ordering::Relaxed);
        }
        let router = Arc::new(Router::new(links.clone(), settings.policy()));
        AppState {
            links,
            router,
            settings: Mutex::new(settings),
            settings_path,
            usage: Mutex::new(Usage::load(&usage_path)),
            usage_path,
            engine: tokio::sync::Mutex::new(None),
            flows: RwLock::new(None),
            busy: AtomicBool::new(false),
            error: Mutex::new(None),
            flow_counts: Mutex::new(FlowCounts::default()),
            default_applied: AtomicU32::new(u32::MAX),
            health_stop: watch::channel(false).0,
        }
    }

    pub fn health_stop(&self) -> watch::Receiver<bool> {
        self.health_stop.subscribe()
    }

    pub fn is_on(&self) -> bool {
        self.flows.read().unwrap().is_some()
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Changes the settings, saves them, and hands the balancer its new policy.
    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) {
        let mut settings = self.settings.lock().unwrap();
        change(&mut settings);
        settings.save(&self.settings_path);
        self.router.set_policy(settings.policy());
    }

    pub fn save_usage(&self) {
        self.usage.lock().unwrap().save(&self.usage_path);
        self.save_capacities();
    }

    /// Gives links that know nothing yet (new this run, or just found) the
    /// capacity saved for them.
    pub fn restore_capacities(&self) {
        let settings = self.settings();
        for link in self.links.all() {
            let saved = settings.link(&link.id).capacity;
            if saved > 0 {
                let _ = link.stats.capacity.compare_exchange(
                    0,
                    saved,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
        }
    }

    /// Saves the links' capacities when one moved by more than a tenth.
    fn save_capacities(&self) {
        let settings = self.settings();
        let current: Vec<(String, u64)> = self
            .links
            .all()
            .iter()
            .map(|l| (l.id.clone(), l.stats.capacity.load(Ordering::Relaxed)))
            .collect();
        let moved = current.iter().any(|(id, now)| {
            let saved = settings.link(id).capacity;
            now.abs_diff(saved) > saved / 10
        });
        if moved {
            self.update_settings(|s| {
                for (id, now) in current {
                    s.links.entry(id).or_default().capacity = now;
                }
            });
        }
    }

    /// The interface NetAgg made the default route, if it made one.
    pub fn applied_default(&self) -> Option<u32> {
        match self.default_applied.load(Ordering::Relaxed) {
            0 | u32::MAX => None,
            index => Some(index),
        }
    }

    /// Points the OS default route at the chosen (or automatically picked)
    /// network. Windows only, and only with administrator rights; otherwise
    /// the choice waits for a run that has them.
    pub fn apply_default_route(&self) {
        #[cfg(windows)]
        {
            if !crate::sys::is_elevated() {
                return;
            }
            let chosen = self.settings().default_link;
            let target = self.links.preferred_default(chosen.as_deref());
            let code = target.unwrap_or(0);
            if self.default_applied.swap(code, Ordering::Relaxed) == code {
                return;
            }
            match crate::sys::prefer_interface(target) {
                Ok(()) => log::info!("default route: {target:?}"),
                Err(err) => {
                    log::warn!("couldn't set the default route: {err}");
                    // Try again on the next refresh.
                    self.default_applied.store(u32::MAX, Ordering::Relaxed);
                }
            }
        }
    }

    /// Starts capturing. Needs administrator rights.
    pub async fn turn_on(&self) -> Result<(), String> {
        let mut engine = self.engine.lock().await;
        if engine.is_some() {
            return Ok(());
        }
        self.busy.store(true, Ordering::Relaxed);
        let settings = self.settings();
        let started = match self.engine_config(&settings) {
            Ok(config) => Engine::start(config, self.links.clone(), self.router.clone()).await,
            Err(err) => Err(err),
        };
        self.busy.store(false, Ordering::Relaxed);
        match started {
            Ok(started) => {
                *self.flows.write().unwrap() = Some(started.flows.clone());
                *self.error.lock().unwrap() = None;
                *engine = Some(started);
                drop(engine);
                self.update_settings(|s| s.on = true);
                Ok(())
            }
            Err(err) => {
                log::error!("could not start the engine: {err}");
                let message = format!("Couldn't turn on: {err}");
                *self.error.lock().unwrap() = Some(message.clone());
                Err(message)
            }
        }
    }

    fn engine_config(&self, settings: &Settings) -> std::io::Result<EngineConfig> {
        let manual = settings.dns_mode == DnsMode::Manual;
        let upstreams = if manual {
            settings.upstreams().map_err(std::io::Error::other)?
        } else {
            Vec::new()
        };
        let resolver = Resolver::new(self.router.clone(), settings.dns_mode, upstreams)?;
        Ok(EngineConfig {
            resolver: Arc::new(resolver),
            enforce_dns: manual && settings.enforce_dns,
        })
    }

    /// Stops capturing. `remember` records that the user turned it off, so
    /// it stays off at the next start.
    pub async fn turn_off(&self, remember: bool) {
        let mut engine = self.engine.lock().await;
        if let Some(running) = engine.take() {
            self.busy.store(true, Ordering::Relaxed);
            *self.flows.write().unwrap() = None;
            if tokio::time::timeout(STOP_TIMEOUT, running.stop())
                .await
                .is_err()
            {
                log::warn!("the engine took too long to stop");
            }
            self.busy.store(false, Ordering::Relaxed);
        }
        drop(engine);
        if remember {
            self.update_settings(|s| s.on = false);
        }
        *self.flow_counts.lock().unwrap() = FlowCounts::default();
    }

    /// Applies settings the engine only reads at start (DNS).
    /// Goes by "on" as saved, not the running engine: a change that arrives
    /// while an earlier one's start is still under way must restart it too,
    /// or the engine keeps the earlier settings. `turn_off` waits for that
    /// start to finish.
    pub async fn restart(&self) -> Result<(), String> {
        if !self.settings().on {
            return Ok(());
        }
        self.turn_off(false).await;
        self.turn_on().await
    }

    /// Cleans up after the packet stack stopped by itself, which would
    /// otherwise leave the PC without internet. Returns true if it had.
    pub async fn check_engine(&self) -> bool {
        let stalled = match self.engine.try_lock() {
            Ok(engine) => engine.as_ref().is_some_and(|e| !e.is_running()),
            // Turning on or off right now.
            Err(_) => false,
        };
        if stalled {
            self.turn_off(false).await;
            *self.error.lock().unwrap() = Some("NetAgg stopped unexpectedly".into());
        }
        stalled
    }
}
