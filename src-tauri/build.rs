fn main() {
    // Only the desktop app is a Tauri app; netagg-server has no Tauri config.
    #[cfg(feature = "gui")]
    tauri_build::build()
}
