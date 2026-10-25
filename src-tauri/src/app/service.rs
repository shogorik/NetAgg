//! Windows: the engine runs in the NetAgg service (SYSTEM, no window), and
//! the tray app, running as the user, asks it for everything over a named
//! pipe: one JSON request per connection, one JSON reply.
//!
//! The service only answers its own executable: the installed `netagg.exe`.
//! Changes that need an administrator (`Request::needs_admin`) are refused
//! from the normal tray app; it starts an elevated copy of itself
//! (`--admin-request`, through UAC) that sends that one request.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer};

use super::control::{self, Caller, Request, NEEDS_ADMIN};
use super::state::AppState;
use super::{logfile, ticker::Ticks};
use crate::net::health;
use crate::{engine, sys};

/// How the installer registers the service: `netagg.exe --service`.
pub const SERVICE_ARG: &str = "--service";
/// Followed by a request, hex-encoded JSON.
pub const ADMIN_REQUEST_ARG: &str = "--admin-request";
/// More than any request needs.
const MAX_REQUEST: u64 = 64 * 1024;
/// Turning on, or restarting for new DNS servers, takes a few seconds.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const ERROR_PIPE_BUSY: i32 = 231;

pub const UNREACHABLE: &str =
    "The NetAgg service isn't running. Reinstall NetAgg, or start the service in Windows' Services.";

/// The service's entry point; returns the process exit code. Outside the
/// service manager (an elevated terminal, for development) it runs in the
/// foreground until Ctrl+C.
pub fn run() -> i32 {
    let body = |stopped: std::sync::mpsc::Receiver<()>| {
        let stop = async move {
            let _ = tokio::task::spawn_blocking(move || stopped.recv()).await;
        };
        serve_blocking(stop)
    };
    match sys::run_service(body) {
        Ok(()) => 0,
        Err(err) if err.raw_os_error() == Some(1063) => serve_blocking(async {
            let _ = tokio::signal::ctrl_c().await;
        }) as i32,
        Err(err) => {
            eprintln!("couldn't start the service: {err}");
            1
        }
    }
}

fn serve_blocking(stop: impl std::future::Future<Output = ()>) -> u32 {
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return 1;
    };
    let code = runtime.block_on(serve(stop));
    // Dropping the runtime would wait for every blocking task, and one
    // still running kept the service "stopping" for good. The engine is
    // off and everything is saved by now.
    runtime.shutdown_timeout(Duration::from_secs(2));
    code
}

async fn serve(stop: impl std::future::Future<Output = ()>) -> u32 {
    let Some(data_dir) = sys::service_data_dir() else {
        return 2;
    };
    logfile::init(&data_dir);
    log::info!(
        "NetAgg service {} started from {:?}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe().ok()
    );
    if !sys::is_elevated() {
        log::error!("the service needs administrator rights");
        return 1;
    }
    engine::clean_up_stale();
    let state = Arc::new(AppState::new(data_dir));
    tokio::spawn(health::run(state.links.clone(), state.health_stop()));
    state.apply_default_route();
    // NetAgg comes back the way it was left, from boot on.
    if let Err(err) = control::handle(&state, Request::Resume, Caller { elevated: true }).await {
        log::warn!("{err}");
    }
    let server = tokio::spawn(listen(state.clone()));

    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut ticks = Ticks::new(&state);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = interval.tick() => ticks.tick(&state).await,
            _ = &mut stop => break,
        }
    }
    server.abort();
    // Gives the PC its own routing back; "on" is kept for the next start.
    state.turn_off(false).await;
    state.save_usage();
    log::info!("NetAgg service stopped");
    0
}

/// Answers the pipe until the service stops.
async fn listen(state: Arc<AppState>) {
    let own_exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return log::error!("can't serve the pipe: {err}"),
    };
    let mut first = true;
    loop {
        // The first instance must be new: if another process already holds
        // the name, it isn't the service, and the app mustn't talk to it.
        // (The app checks too.)
        let created = sys::create_pipe(first);
        let server = match created {
            Ok(server) => server,
            Err(err) => {
                log::error!("can't serve the pipe: {err}");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        first = false;
        if let Err(err) = server.connect().await {
            log::warn!("pipe: {err}");
            continue;
        }
        let state = state.clone();
        let own_exe = own_exe.clone();
        tokio::spawn(async move {
            if let Err(err) = answer(server, &state, &own_exe).await {
                log::warn!("pipe: {err}");
            }
        });
    }
}

async fn answer(pipe: NamedPipeServer, state: &AppState, own_exe: &Path) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    let client = sys::pipe_client(pipe.as_raw_handle())?;
    if !same_path(&client.image, own_exe) {
        log::warn!("refused a request from {}", client.image.display());
        return Ok(());
    }
    let mut reader = BufReader::new(pipe);
    let mut line = String::new();
    (&mut reader).take(MAX_REQUEST).read_line(&mut line).await?;
    let reply = match serde_json::from_str::<Request>(&line) {
        Ok(request) => {
            if request.needs_admin() && client.elevated {
                log::info!("administrator request: {request:?}");
            }
            let caller = Caller {
                elevated: client.elevated,
            };
            tokio::time::timeout(CALL_TIMEOUT, control::handle(state, request, caller))
                .await
                .unwrap_or_else(|_| Err("NetAgg took too long to answer".into()))
        }
        Err(err) => Err(format!("not a request: {err}")),
    };
    let mut out = serde_json::to_vec(&reply)?;
    out.push(b'\n');
    let pipe = reader.get_mut();
    pipe.write_all(&out).await?;
    pipe.flush().await
}

/// Windows paths ignore case.
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
}

/// Sends one request to the service and returns its reply.
async fn send(request: &Request) -> Result<Value, String> {
    let exchange = async {
        let mut pipe = loop {
            match ClientOptions::new().open(sys::PIPE_NAME) {
                Ok(pipe) => break pipe,
                Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(_) => return Err(UNREACHABLE.to_owned()),
            }
        };
        // Whoever holds the name must be the service, not a program that took
        // the name while the service was stopped.
        let trusted = {
            use std::os::windows::io::AsRawHandle;
            sys::pipe_owned_by_admins(pipe.as_raw_handle()).map_err(|e| e.to_string())?
        };
        if !trusted {
            return Err(
                "Another program answers on NetAgg's pipe instead of the NetAgg service.".into(),
            );
        }
        let mut line = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        line.push(b'\n');
        pipe.write_all(&line).await.map_err(|e| e.to_string())?;
        let mut reply = String::new();
        BufReader::new(pipe)
            .read_line(&mut reply)
            .await
            .map_err(|e| e.to_string())?;
        if reply.is_empty() {
            // It hung up: it didn't recognise this program.
            return Err("The NetAgg service refused this copy of NetAgg.".to_owned());
        }
        serde_json::from_str::<Result<Value, String>>(&reply).map_err(|e| e.to_string())?
    };
    tokio::time::timeout(CALL_TIMEOUT + Duration::from_secs(5), exchange)
        .await
        .unwrap_or_else(|_| Err("The NetAgg service didn't answer.".into()))
}

/// The tray app's way to the engine. A change that needs an administrator
/// is checked by the service first, then sent again from an elevated copy
/// of NetAgg (the UAC prompt).
pub async fn call(request: Request) -> Result<Value, String> {
    match send(&request).await {
        Err(err) if err == NEEDS_ADMIN && request.needs_admin() => {
            let line = serde_json::to_string(&request).map_err(|e| e.to_string())?;
            let args = format!("{ADMIN_REQUEST_ARG} {}", hex(line.as_bytes()));
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let code = tokio::task::spawn_blocking(move || {
                sys::run_elevated(&exe.to_string_lossy(), &args)
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|_| {
                "The change needs administrator approval, which wasn't given.".to_owned()
            })?;
            if code != 0 {
                return Err(format!(
                    "The change wasn't made (code {code}); NetAgg's service log has the details."
                ));
            }
            send(&Request::Status).await
        }
        reply => reply,
    }
}

/// The elevated copy: sends the one request it was given. Returns the exit
/// code: 0 done, 1 refused or failed, 2 not a request.
pub fn run_admin_request(args: &[String]) -> i32 {
    let Some(request) = args
        .iter()
        .skip_while(|a| *a != ADMIN_REQUEST_ARG)
        .nth(1)
        .and_then(|h| unhex(h))
        .and_then(|bytes| serde_json::from_slice::<Request>(&bytes).ok())
        .filter(Request::needs_admin)
    else {
        return 2;
    };
    let Ok(runtime) = tokio::runtime::Runtime::new() else {
        return 1;
    };
    match runtime.block_on(send(&request)) {
        Ok(_) => 0,
        Err(_) => 1,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_survive_the_command_line() {
        let line =
            r#"{"cmd":"setOption","change":{"dns":["tls://dns.adguard-dns.com#94.140.14.14"]}}"#;
        assert_eq!(unhex(&hex(line.as_bytes())).unwrap(), line.as_bytes());
        assert!(unhex("abc").is_none());
        assert!(unhex("zz").is_none());
        assert!(unhex("é1").is_none());
    }

    #[test]
    fn the_elevated_copy_only_sends_administrator_changes() {
        let args = |line: &str| {
            vec![
                "netagg.exe".to_owned(),
                ADMIN_REQUEST_ARG.to_owned(),
                hex(line.as_bytes()),
            ]
        };
        assert_eq!(
            run_admin_request(&args(r#"{"cmd":"setEnabled","on":false,"remember":true}"#)),
            2
        );
        assert_eq!(run_admin_request(&args("not json")), 2);
        assert_eq!(run_admin_request(&["netagg.exe".to_owned()]), 2);
    }

    #[test]
    fn paths_compare_without_case() {
        assert!(same_path(
            Path::new(r"D:\Program Files\NetAgg\netagg.exe"),
            Path::new(r"d:\program files\netagg\NETAGG.EXE")
        ));
        assert!(!same_path(
            Path::new(r"D:\Program Files\NetAgg\netagg.exe"),
            Path::new(r"C:\Users\me\Downloads\netagg.exe")
        ));
    }
}
