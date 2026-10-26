//! Whether a newer netagg-server is out. The WebUI only tells; installing
//! is the package manager's job (the NetAgg apt repository, or the .deb from
//! the release page). Asked of GitHub's API, never more than once in
//! `CACHE_FOR`, and only when the person checks or turned on checking at start.

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

const HOST: &str = "api.github.com";
const PATH: &str = "/repos/shogorik/NetAgg/releases/latest";
const CACHE_FOR: Duration = Duration::from_secs(10 * 60);
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_REPLY: usize = 512 * 1024;
/// The release asset that says a release has a server package.
const SERVER_ASSET: &str = "netagg-server_";

/// What the WebUI shows.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub current: String,
    /// The newest release with a netagg-server package, if it's newer.
    pub available: Option<String>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct UpdateCheck {
    last: Mutex<Option<(Instant, UpdateInfo)>>,
}

impl UpdateCheck {
    pub async fn check(self: &Arc<Self>) -> UpdateInfo {
        if let Some((at, info)) = self.last.lock().unwrap().as_ref() {
            if at.elapsed() < CACHE_FOR {
                return info.clone();
            }
        }
        let current = env!("CARGO_PKG_VERSION").to_owned();
        let info = match tokio::time::timeout(TIMEOUT, latest_release()).await {
            Ok(Ok(release)) => UpdateInfo {
                available: newer_server(&release, &current),
                current,
                error: None,
            },
            Ok(Err(err)) => failed(current, err.to_string()),
            Err(_) => failed(current, "GitHub didn't answer in time".into()),
        };
        *self.last.lock().unwrap() = Some((Instant::now(), info.clone()));
        info
    }
}

fn failed(current: String, error: String) -> UpdateInfo {
    log::warn!("couldn't check for updates: {error}");
    UpdateInfo {
        current,
        available: None,
        error: Some(format!("Couldn't check for updates: {error}")),
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
}

/// The release's version, if it has a server package and is newer than
/// `current`.
fn newer_server(release: &Release, current: &str) -> Option<String> {
    let version = release.tag_name.trim_start_matches('v');
    let has_server = release
        .assets
        .iter()
        .any(|a| a.name.starts_with(SERVER_ASSET));
    (has_server && is_newer(version, current)).then(|| version.to_owned())
}

/// Dotted numbers compared as numbers: 1.10.0 is newer than 1.9.3.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parts = |v: &str| -> Option<Vec<u64>> { v.split('.').map(|p| p.parse().ok()).collect() };
    match (parts(candidate), parts(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

async fn latest_release() -> io::Result<Release> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .and_then(|b| b.with_platform_verifier())
        .map_err(io::Error::other)?
        .with_no_client_auth();
    let tcp = TcpStream::connect((HOST, 443)).await?;
    let name = ServerName::try_from(HOST).map_err(io::Error::other)?;
    let tls = TlsConnector::from(Arc::new(config))
        .connect(name, tcp)
        .await?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
        .await
        .map_err(io::Error::other)?;
    tokio::spawn(connection);
    let request = hyper::Request::get(PATH)
        .header(hyper::header::HOST, HOST)
        // GitHub's API refuses requests without one.
        .header(
            hyper::header::USER_AGENT,
            concat!("netagg-server/", env!("CARGO_PKG_VERSION")),
        )
        .header(hyper::header::ACCEPT, "application/vnd.github+json")
        .body(Empty::<Bytes>::new())
        .map_err(io::Error::other)?;
    let response = sender
        .send_request(request)
        .await
        .map_err(io::Error::other)?;
    let status = response.status();
    let body = Limited::new(response.into_body(), MAX_REPLY)
        .collect()
        .await
        .map_err(io::Error::other)?
        .to_bytes();
    if !status.is_success() {
        return Err(io::Error::other(format!("GitHub answered {status}")));
    }
    serde_json::from_slice(&body).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &[&str]) -> Release {
        Release {
            tag_name: tag.into(),
            assets: assets.iter().map(|n| Asset { name: (*n).into() }).collect(),
        }
    }

    #[test]
    fn versions_compare_as_numbers() {
        assert!(is_newer("1.10.0", "1.9.3"));
        assert!(is_newer("2.0.0", "1.4.0"));
        assert!(!is_newer("1.4.0", "1.4.0"));
        assert!(!is_newer("1.3.10", "1.4.0"));
        assert!(!is_newer("2.0.0-rc1", "1.4.0"));
    }

    #[test]
    fn only_releases_with_a_server_package_count() {
        let server = release(
            "v2.0.0",
            &[
                "NetAgg_2.0.0_x64-setup.exe",
                "netagg-server_2.0.0_amd64.deb",
            ],
        );
        assert_eq!(newer_server(&server, "1.4.0").as_deref(), Some("2.0.0"));
        let windows_only = release("v2.0.1", &["NetAgg_2.0.1_x64-setup.exe"]);
        assert_eq!(newer_server(&windows_only, "2.0.0"), None);
        assert_eq!(newer_server(&server, "2.0.0"), None);
    }

    #[test]
    fn reads_githubs_reply() {
        let json = br#"{"tag_name":"v1.4.0","name":"NetAgg 1.4.0","assets":[{"name":"latest.json","size":1}]}"#;
        let parsed: Release = serde_json::from_slice(json).unwrap();
        assert_eq!(parsed.tag_name, "v1.4.0");
        assert_eq!(parsed.assets.len(), 1);
    }

    /// Asks GitHub for real: `cargo test --features server live_update -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn live_update_check() {
        let release = latest_release().await.unwrap();
        assert!(release.tag_name.starts_with('v'));
    }
}
