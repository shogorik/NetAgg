mod app;
#[cfg(feature = "gui")]
mod desktop;
mod engine;
mod net;
mod sys;

#[cfg(feature = "gui")]
pub use desktop::run;

use app::headless;

/// The modes NetAgg runs in without a window (the headless runner, the
/// service and its helpers), chosen by an argument. Returns the exit code
/// of the one asked for, or `None` for the normal app.
fn run_mode() -> Option<i32> {
    let has_arg = |arg: &str| std::env::args().any(|a| a == arg);
    if has_arg(headless::HEADLESS_ARG) {
        return Some(headless::run());
    }
    #[cfg(any(windows, target_os = "linux"))]
    if has_arg(app::service::SERVICE_ARG) {
        return Some(app::service::run());
    }
    #[cfg(windows)]
    if has_arg(app::service::PREPARE_DATA_DIR_ARG) {
        return Some(app::service::run_prepare_data_dir());
    }
    #[cfg(target_os = "linux")]
    if has_arg(app::service::CLEAN_UP_ARG) {
        // To the journal, as the service logs.
        log::set_logger(&headless::StderrLog).ok();
        log::set_max_level(log::LevelFilter::Info);
        engine::clean_up_stale();
        log::info!("clean-up done");
        return Some(0);
    }
    #[cfg(any(windows, target_os = "linux"))]
    if has_arg(app::service::ADMIN_REQUEST_ARG) {
        let args: Vec<String> = std::env::args().collect();
        return Some(app::service::run_admin_request(&args));
    }
    #[cfg(target_os = "macos")]
    if has_arg(engine::HELPER_ARG) {
        let args: Vec<String> = std::env::args().collect();
        return Some(engine::run_helper(&args));
    }
    None
}

/// netagg-server: the service, its WebUI, setting the WebUI's password, and
/// the headless modes.
#[cfg(feature = "server")]
pub fn run_server() -> i32 {
    #[cfg(target_os = "linux")]
    {
        use app::webui;
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == webui::WEBUI_ARG) {
            return webui::run(&args);
        }
        if args.get(1).is_some_and(|a| a == webui::PASSWORD_ARG) {
            return webui::run_password(&args);
        }
    }
    run_mode().unwrap_or_else(|| {
        eprintln!(
            "usage: netagg-server --service | --webui | webui-password | {}",
            headless::HEADLESS_ARG
        );
        2
    })
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

    #[cfg(feature = "gui")]
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
