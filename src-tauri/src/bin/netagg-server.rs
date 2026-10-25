//! NetAgg for headless Linux servers: the service and its WebUI, without the
//! tray app (no Tauri, GTK or WebKit).

fn main() {
    std::process::exit(netagg_lib::run_server())
}
