//! The Tauri side: the popup's commands, the ticker and the tray, which ask
//! the engine through a backend: shared state in this process, or the
//! NetAgg service. Also the engine's side of those requests (`control`),
//! the service itself, and the headless runner, which drives the same state
//! without Tauri. The Tauri parts are only in the desktop app (`gui`).

#[cfg(feature = "gui")]
pub mod autostart;
#[cfg(feature = "gui")]
pub mod backend;
#[cfg(feature = "gui")]
pub mod commands;
pub mod control;
pub mod headless;
#[cfg(any(windows, feature = "gui"))]
pub mod logfile;
#[cfg(feature = "gui")]
pub mod popup;
#[cfg(any(windows, target_os = "linux"))]
pub mod service;
pub mod settings;
pub mod state;
pub mod status;
#[cfg(feature = "gui")]
pub mod ticker;
pub mod ticks;
#[cfg(all(feature = "gui", not(target_os = "linux")))]
pub mod tray;
#[cfg(all(feature = "gui", target_os = "linux"))]
#[path = "tray_linux.rs"]
pub mod tray;
pub mod usage;
#[cfg(all(feature = "server", target_os = "linux"))]
pub mod webui;
