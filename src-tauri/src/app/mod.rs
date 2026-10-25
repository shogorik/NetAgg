//! The Tauri side: the popup's commands, the ticker and the tray, which ask
//! the engine through a backend: shared state in this process, or the
//! Windows service. Also the engine's side of those requests (`control`),
//! the service itself, and the headless runner, which drives the same state
//! without Tauri.

pub mod autostart;
pub mod backend;
pub mod commands;
pub mod control;
pub mod headless;
pub mod logfile;
pub mod popup;
#[cfg(windows)]
pub mod service;
pub mod settings;
pub mod state;
pub mod status;
pub mod ticker;
pub mod tray;
pub mod usage;
