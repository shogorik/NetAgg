//! netagg-server's WebUI (`netagg-server --webui`, `netagg-webui.service`):
//! the popup in a browser, over HTTPS, for headless Linux servers. It runs
//! as its own user, without root or capabilities, and forwards each request
//! of a signed-in browser to the NetAgg service over its control socket.
//! The service counts this user as administrator, so the WebUI is the
//! boundary: no session, no request.
//!
//! Only this machine, its own networks and private or Tailscale addresses
//! are answered; the internet never gets as far as the sign-in page.

mod password;
mod tls;

use std::collections::HashMap;
use std::convert::Infallible;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request as HttpRequest, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use super::control::Request;
use super::service;

pub use password::{run as run_password, PASSWORD_ARG};

pub const WEBUI_ARG: &str = "--webui";
/// The built popup, for development: `--www <dist folder>`.
const WWW_ARG: &str = "--www";
const WWW_DIR: &str = "/usr/share/netagg-server/www";
const PORT: u16 = 8443;
const COOKIE: &str = "netagg_session";
/// A session ends after this long without a request.
const IDLE: Duration = Duration::from_secs(12 * 60 * 60);
const MAX_BODY: usize = 64 * 1024;
/// Failed sign-ins from one address before it has to wait.
const MAX_FAILURES: u32 = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(5 * 60);

/// The popup is a single page; nothing is loaded from elsewhere.
const APP_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; \
    frame-ancestors 'none'; form-action 'self'";
const LOGIN_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; \
    frame-ancestors 'none'; base-uri 'none'";

type Body = Full<Bytes>;

struct Session {
    last_seen: Instant,
    /// The password's hash when it was opened: a new password ends it.
    hash: String,
}

struct WebUi {
    files: HashMap<String, (Bytes, &'static str)>,
    sessions: Mutex<HashMap<String, Session>>,
    failures: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

/// Runs until stopped; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    log::set_logger(&super::headless::StderrLog).ok();
    log::set_max_level(log::LevelFilter::Info);
    let www = args
        .iter()
        .skip_while(|a| *a != WWW_ARG)
        .nth(1)
        .map_or_else(|| PathBuf::from(WWW_DIR), PathBuf::from);
    let state_dir = std::env::var_os("STATE_DIRECTORY")
        .map_or_else(|| PathBuf::from("/var/lib/netagg-web"), PathBuf::from);
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("couldn't start: {err}");
            return 1;
        }
    };
    match runtime.block_on(serve(&www, &state_dir)) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("WebUI: {err}");
            1
        }
    }
}

async fn serve(www: &Path, state_dir: &Path) -> io::Result<()> {
    let tls = TlsAcceptor::from(tls::server_config(state_dir)?);
    let webui = Arc::new(WebUi {
        files: load_files(www)?,
        sessions: Mutex::default(),
        failures: Mutex::default(),
    });
    // One socket for both families.
    let listener = TcpListener::bind((Ipv6Addr::UNSPECIFIED, PORT)).await?;
    log::info!("WebUI on https://<this machine>:{PORT}/");
    let shutdown = super::headless::shutdown_requested();
    tokio::pin!(shutdown);
    loop {
        let (tcp, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(err) => {
                    log::warn!("WebUI: {err}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = &mut shutdown => return Ok(()),
        };
        let client = canonical(peer.ip());
        if !allowed_client(client, &local_networks()) {
            // Not even a TLS handshake for the internet.
            continue;
        }
        let tls = tls.clone();
        let webui = webui.clone();
        tokio::spawn(async move {
            let Ok(Ok(stream)) =
                tokio::time::timeout(Duration::from_secs(10), tls.accept(tcp)).await
            else {
                return;
            };
            let service = service_fn(move |req| {
                let webui = webui.clone();
                async move { Ok::<_, Infallible>(webui.handle(req, client).await) }
            });
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

impl WebUi {
    async fn handle(&self, req: HttpRequest<Incoming>, client: IpAddr) -> Response<Body> {
        let path = req.uri().path().to_owned();
        let mut response = match (req.method(), path.as_str()) {
            (&Method::GET, "/login") => login_page(None),
            (&Method::POST, "/login") => match same_origin(&req) {
                true => self.login(req, client).await,
                false => plain(StatusCode::FORBIDDEN, "Cross-site request refused."),
            },
            (&Method::POST, "/logout") => {
                if same_origin(&req) {
                    if let Some(token) = session_cookie(&req) {
                        self.sessions.lock().unwrap().remove(&token);
                    }
                }
                let mut response = redirect("/login");
                response
                    .headers_mut()
                    .insert(header::SET_COOKIE, cookie_header("", 0));
                response
            }
            _ if !self.signed_in(&req) => match req.method() == Method::GET && path == "/" {
                true => redirect("/login"),
                false => plain(StatusCode::UNAUTHORIZED, "Sign in first."),
            },
            (&Method::POST, "/api") => match same_origin(&req) {
                true => self.api(req).await,
                false => plain(StatusCode::FORBIDDEN, "Cross-site request refused."),
            },
            (&Method::GET, _) => self.file(&path),
            _ => plain(StatusCode::NOT_FOUND, "Not found."),
        };
        let headers = response.headers_mut();
        headers.insert(
            "x-content-type-options",
            HeaderValue::from_static("nosniff"),
        );
        headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
        headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
        headers
            .entry(header::CACHE_CONTROL)
            .or_insert(HeaderValue::from_static("no-store"));
        response
    }

    fn signed_in(&self, req: &HttpRequest<Incoming>) -> bool {
        let Some(token) = session_cookie(req) else {
            return false;
        };
        let current = password::stored();
        let mut sessions = self.sessions.lock().unwrap();
        let now = Instant::now();
        sessions.retain(|_, s| {
            now.duration_since(s.last_seen) < IDLE && Some(&s.hash) == current.as_ref()
        });
        match sessions.get_mut(&token) {
            Some(session) => {
                session.last_seen = now;
                true
            }
            None => false,
        }
    }

    async fn login(&self, req: HttpRequest<Incoming>, client: IpAddr) -> Response<Body> {
        // Counted before the check, so guesses sent in parallel can't get past the limit.
        let Some(attempt) = self.attempt(client) else {
            return login_page(Some(
                "Too many wrong passwords. Wait a few minutes, then try again.",
            ));
        };
        let Some(hash) = password::stored() else {
            return login_page(None);
        };
        let Some(form) = read_body(req).await else {
            return plain(StatusCode::PAYLOAD_TOO_LARGE, "Too large.");
        };
        let given = form_value(&form, "password").unwrap_or_default();
        let checked = hash.clone();
        let ok = tokio::task::spawn_blocking(move || password::verify(&given, &checked))
            .await
            .unwrap_or(false);
        if !ok {
            log::warn!("wrong WebUI password from {client} ({attempt} in a row)");
            // Wrong guesses cost time too.
            tokio::time::sleep(Duration::from_secs(1)).await;
            return login_page(Some("That password isn't right."));
        }
        self.failures.lock().unwrap().remove(&client);
        let token = match new_token() {
            Ok(token) => token,
            Err(err) => return plain(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        };
        self.sessions.lock().unwrap().insert(
            token.clone(),
            Session {
                last_seen: Instant::now(),
                hash,
            },
        );
        log::info!("signed in from {client}");
        let mut response = redirect("/");
        response
            .headers_mut()
            .insert(header::SET_COOKIE, cookie_header(&token, IDLE.as_secs()));
        response
    }

    /// Counts a sign-in attempt from `client` and returns its number, or
    /// `None` while that address has to wait. A right password clears it.
    fn attempt(&self, client: IpAddr) -> Option<u32> {
        let mut failures = self.failures.lock().unwrap();
        failures.retain(|_, (_, since)| since.elapsed() < FAILURE_WINDOW);
        let entry = failures.entry(client).or_insert((0, Instant::now()));
        if entry.0 >= MAX_FAILURES {
            return None;
        }
        entry.0 += 1;
        Some(entry.0)
    }

    /// One request for the service, as the popup would send it.
    async fn api(&self, req: HttpRequest<Incoming>) -> Response<Body> {
        let Some(body) = read_body(req).await else {
            return plain(StatusCode::PAYLOAD_TOO_LARGE, "Too large.");
        };
        let reply = match serde_json::from_slice::<Request>(&body) {
            Ok(request) => service::send(&request).await,
            Err(err) => Err(format!("not a request: {err}")),
        };
        match serde_json::to_vec(&reply) {
            Ok(json) => with_type(StatusCode::OK, json.into(), "application/json"),
            Err(err) => plain(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        }
    }

    fn file(&self, path: &str) -> Response<Body> {
        let key = if path == "/" { "/index.html" } else { path };
        match self.files.get(key) {
            Some((bytes, kind)) => {
                let mut response = with_type(StatusCode::OK, bytes.clone(), kind);
                if *kind == "text/html; charset=utf-8" {
                    response
                        .headers_mut()
                        .insert("content-security-policy", HeaderValue::from_static(APP_CSP));
                }
                response
            }
            None => plain(StatusCode::NOT_FOUND, "Not found."),
        }
    }
}

/// The built popup, read once at start: requests can only ever get these.
fn load_files(root: &Path) -> io::Result<HashMap<String, (Bytes, &'static str)>> {
    let mut files = HashMap::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                dirs.push(path);
            } else if kind.is_file() {
                let rel = path.strip_prefix(root).map_err(io::Error::other)?;
                let url = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
                let mut bytes = Vec::new();
                std::fs::File::open(&path)?.read_to_end(&mut bytes)?;
                files.insert(url, (Bytes::from(bytes), content_type(&path)));
            }
        }
    }
    if !files.contains_key("/index.html") {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no index.html in {}", root.display()),
        ));
    }
    Ok(files)
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// A POST must come from a page this server sent: the browser's Origin
/// names the address the page was loaded from.
fn same_origin(req: &HttpRequest<Incoming>) -> bool {
    let header = |name| req.headers().get(name).and_then(|v| v.to_str().ok());
    match (header(header::ORIGIN), header(header::HOST)) {
        (Some(origin), Some(host)) => origin == format!("https://{host}"),
        _ => false,
    }
}

fn session_cookie(req: &HttpRequest<Incoming>) -> Option<String> {
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            pair.trim()
                .strip_prefix(&format!("{COOKIE}="))
                .map(str::to_owned)
        })
        .filter(|token| token.len() == 64)
}

fn cookie_header(token: &str, max_age: u64) -> HeaderValue {
    let cookie =
        format!("{COOKIE}={token}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Strict");
    HeaderValue::from_str(&cookie).unwrap_or(HeaderValue::from_static(""))
}

/// 32 random bytes from the kernel, as hex.
fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The body, unless it's larger than any request needs.
async fn read_body(req: HttpRequest<Incoming>) -> Option<Bytes> {
    let body = Limited::new(req.into_body(), MAX_BODY).collect().await;
    body.ok().map(|body| body.to_bytes())
}

/// One field of an `application/x-www-form-urlencoded` body.
fn form_value(body: &[u8], name: &str) -> Option<String> {
    url::form_urlencoded::parse(body)
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn with_type(status: StatusCode, body: Bytes, kind: &'static str) -> Response<Body> {
    let mut response = Response::new(Full::new(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    response
}

fn plain(status: StatusCode, text: &str) -> Response<Body> {
    with_type(
        status,
        Bytes::from(text.to_owned()),
        "text/plain; charset=utf-8",
    )
}

fn redirect(to: &'static str) -> Response<Body> {
    let mut response = plain(StatusCode::SEE_OTHER, "");
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static(to));
    response
}

fn login_page(error: Option<&str>) -> Response<Body> {
    let set = password::stored().is_some();
    let body = match (set, error) {
        (false, _) => format!(
            "<p>No WebUI password has been set yet. On the server, run:</p>\
             <pre>sudo netagg-server {PASSWORD_ARG}</pre>"
        ),
        (true, error) => format!(
            "{}<form method=\"post\" action=\"/login\">\
             <label for=\"p\">Password</label>\
             <input id=\"p\" name=\"password\" type=\"password\" autocomplete=\"current-password\" autofocus required>\
             <button type=\"submit\">Sign in</button></form>\
             <p class=\"hint\">Forgot it? On the server, set a new one: \
             <code>sudo netagg-server {PASSWORD_ARG}</code></p>",
            error.map_or(String::new(), |e| format!("<p class=\"error\">{e}</p>"))
        ),
    };
    let page = format!("{LOGIN_HEAD}{body}</main></body></html>");
    let mut response = with_type(StatusCode::OK, page.into(), "text/html; charset=utf-8");
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_static(LOGIN_CSP),
    );
    response
}

const LOGIN_HEAD: &str = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>NetAgg</title><style>\
:root{color-scheme:dark;--bg:#14161a;--card:#1d2026;--text:#e8eaed;--muted:#9aa0a6;--accent:#4cc2ff;--err:#ff6b6b}\
body{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--text);\
font:15px/1.5 system-ui,-apple-system,'Segoe UI',sans-serif}\
main{width:min(340px,calc(100vw - 32px));background:var(--card);border-radius:12px;padding:24px}\
h1{font-size:20px;margin:0 0 16px}label{display:block;margin-bottom:6px;color:var(--muted)}\
input{box-sizing:border-box;width:100%;padding:10px;border-radius:8px;border:1px solid #3a3f47;background:#121418;color:var(--text);font:inherit}\
button{margin-top:14px;width:100%;padding:10px;border:0;border-radius:8px;background:var(--accent);color:#0b1a24;font:inherit;font-weight:600;cursor:pointer}\
pre,code{background:#121418;border-radius:6px;padding:2px 6px}pre{padding:10px;overflow-x:auto}\
.error{color:var(--err)}.hint{color:var(--muted);font-size:13px;margin-top:16px}\
</style></head><body><main><h1>NetAgg</h1>";

/// IPv4 clients reach the dual-stack socket as `::ffff:a.b.c.d`.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// The networks this machine is directly on: (address, prefix length).
fn local_networks() -> Vec<(IpAddr, u8)> {
    netdev::get_interfaces()
        .into_iter()
        .flat_map(|itf| {
            let v4 = itf
                .ipv4
                .iter()
                .map(|n| (IpAddr::V4(n.addr()), n.prefix_len()));
            let v6 = itf
                .ipv6
                .iter()
                .map(|n| (IpAddr::V6(n.addr()), n.prefix_len()));
            v4.chain(v6).collect::<Vec<_>>()
        })
        .collect()
}

/// This machine, a network it's directly on, or a private, link-local or
/// Tailscale (100.64.0.0/10) address. Never the internet at large.
fn allowed_client(ip: IpAddr, local: &[(IpAddr, u8)]) -> bool {
    let private = match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || in_net(ip, IpAddr::V4(Ipv4Addr::new(100, 64, 0, 0)), 10)
        }
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    };
    // A default route's /0 isn't a network the machine is on.
    private
        || local
            .iter()
            .any(|&(net, len)| len > 0 && in_net(ip, net, len))
}

fn in_net(ip: IpAddr, net: IpAddr, len: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(ip), IpAddr::V4(net)) => {
            let mask = u32::MAX
                .checked_shl(32 - u32::from(len.min(32)))
                .unwrap_or(0);
            u32::from(ip) & mask == u32::from(net) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(net)) => {
            let mask = u128::MAX
                .checked_shl(128 - u32::from(len.min(128)))
                .unwrap_or(0);
            u128::from(ip) & mask == u128::from(net) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn answers_local_and_private_clients_only() {
        let local = [(ip("198.51.100.12"), 24), (ip("0.0.0.0"), 0)];
        for ok in [
            "127.0.0.1",
            "10.1.2.3",
            "172.20.0.5",
            "192.168.1.9",
            "169.254.3.3",
            "100.101.102.103",
            "198.51.100.40",
            "::1",
            "fd00::5",
            "fe80::1",
        ] {
            assert!(allowed_client(ip(ok), &local), "{ok}");
        }
        for refused in [
            "8.8.8.8",
            "13.0.1.40",
            "100.128.0.1",
            "2a02:2f0c::1",
            "89.42.42.72",
        ] {
            assert!(!allowed_client(ip(refused), &local), "{refused}");
        }
    }

    #[test]
    fn five_tries_then_the_address_waits() {
        let webui = WebUi {
            files: HashMap::new(),
            sessions: Mutex::default(),
            failures: Mutex::default(),
        };
        let guesser = ip("198.51.100.99");
        let tries: Vec<_> = (0..6).map(|_| webui.attempt(guesser)).collect();
        assert_eq!(tries, [Some(1), Some(2), Some(3), Some(4), Some(5), None]);
        // Another address isn't held up.
        assert_eq!(webui.attempt(ip("198.51.100.40")), Some(1));
    }

    #[test]
    fn ipv4_through_the_dual_stack_socket_is_ipv4() {
        assert_eq!(canonical(ip("::ffff:198.51.100.40")), ip("198.51.100.40"));
        assert_eq!(canonical(ip("fe80::1")), ip("fe80::1"));
    }

    #[test]
    fn tokens_are_long_and_unique() {
        let a = new_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, new_token().unwrap());
    }

    #[test]
    fn reads_the_password_field() {
        assert_eq!(
            form_value(b"password=a%26b+c%3D", "password").as_deref(),
            Some("a&b c=")
        );
        assert_eq!(form_value(b"other=1", "password"), None);
    }

    #[test]
    fn the_cookie_cannot_be_read_by_scripts_or_sent_cross_site() {
        let value = cookie_header(&"a".repeat(64), 60);
        let text = value.to_str().unwrap();
        for part in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(text.contains(part), "{text}");
        }
    }

    #[test]
    fn serves_only_the_built_files() {
        let dir = std::env::temp_dir().join(format!("netagg-www-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), "<html>").unwrap();
        std::fs::write(dir.join("assets/app.js"), "x").unwrap();
        let files = load_files(&dir).unwrap();
        let mut names: Vec<&str> = files.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names, ["/assets/app.js", "/index.html"]);
        assert_eq!(files["/assets/app.js"].1, "text/javascript; charset=utf-8");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
