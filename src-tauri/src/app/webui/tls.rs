//! The WebUI's certificate: self-signed, made on the first start and kept in
//! the WebUI's state folder (its key readable only by the WebUI's user).
//! Browsers warn about it once; the connection is still encrypted.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;

const CERT: &str = "cert.pem";
const KEY: &str = "key.pem";

pub fn server_config(dir: &Path) -> io::Result<Arc<ServerConfig>> {
    if !dir.join(CERT).is_file() || !dir.join(KEY).is_file() {
        create(dir)?;
    }
    let certs = CertificateDer::pem_file_iter(dir.join(CERT))
        .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
        .map_err(io::Error::other)?;
    let key = PrivateKeyDer::from_pem_file(dir.join(KEY)).map_err(io::Error::other)?;
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(io::Error::other)?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(io::Error::other)?;
    Ok(Arc::new(config))
}

/// Names the certificate after this machine, so it matches the address
/// people type.
fn create(dir: &Path) -> io::Result<()> {
    let mut names = vec!["localhost".to_owned()];
    if let Ok(host) = std::fs::read_to_string("/etc/hostname") {
        let host = host.trim();
        if !host.is_empty() {
            names.push(host.to_owned());
            names.push(format!("{host}.local"));
        }
    }
    let made = rcgen::generate_simple_self_signed(names).map_err(io::Error::other)?;
    std::fs::create_dir_all(dir)?;
    write_private(&dir.join(KEY), made.signing_key.serialize_pem().as_bytes())?;
    std::fs::write(dir.join(CERT), made.cert.pem())?;
    log::info!("made the WebUI's certificate in {}", dir.display());
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_and_loads_a_certificate() {
        let dir = std::env::temp_dir().join(format!("netagg-webui-tls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        server_config(&dir).unwrap();
        let key_mode = std::fs::metadata(dir.join(KEY))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(key_mode & 0o777, 0o600);
        // Kept: the second start uses the same one.
        let cert = std::fs::read(dir.join(CERT)).unwrap();
        server_config(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join(CERT)).unwrap(), cert);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
