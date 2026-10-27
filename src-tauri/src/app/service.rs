//! The engine runs in the NetAgg service, and the tray app (and on a Linux
//! server, the WebUI) asks it for everything: one JSON request per
//! connection, one JSON reply. Windows: a SYSTEM service and a named pipe.
//! Linux: `netagg.service` (root, systemd) and a Unix socket.
//!
//! The service only answers its own executable: the installed `netagg`.
//! Changes that need an administrator (`Request::needs_admin`) are refused
//! from the normal tray app; it starts an elevated copy of itself
//! (`--admin-request`, through UAC or polkit) that sends that one request.

#[cfg(windows)]
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;

#[cfg(feature = "gui")]
use super::control::NEEDS_ADMIN;
use super::control::{self, Caller, Request};
#[cfg(windows)]
use super::logfile;
use super::state::AppState;
use super::ticks::Ticks;
use crate::net::health;
use crate::{engine, sys};

/// How the installer registers the service: `netagg.exe --service`.
pub const SERVICE_ARG: &str = "--service";
/// The installer's step that makes the data folder safe (`sys::prepare_data_dir`).
#[cfg(windows)]
pub const PREPARE_DATA_DIR_ARG: &str = "--prepare-data-dir";
/// systemd's `ExecStopPost=`: removes what a service that crashed left
/// (the DNS block outlives it on Linux).
#[cfg(target_os = "linux")]
pub const CLEAN_UP_ARG: &str = "--clean-up";
/// Followed by a request, hex-encoded JSON, or by `-` to read it from
/// standard input (Linux: pkexec's command line shows in every user's
/// process list and in the journal).
pub const ADMIN_REQUEST_ARG: &str = "--admin-request";
/// More than any request needs.
const MAX_REQUEST: u64 = 64 * 1024;
/// Turning on takes a few seconds.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);
#[cfg(windows)]
const ERROR_PIPE_BUSY: i32 = 231;

#[cfg(windows)]
pub const UNREACHABLE: &str =
    "The NetAgg service isn't running. Reinstall NetAgg, or start the service in Windows' Services.";
#[cfg(target_os = "linux")]
pub const UNREACHABLE: &str =
    "The NetAgg service isn't running. Start it with: sudo systemctl start netagg";

/// The service's entry point; returns the process exit code. Outside the
/// service manager (an elevated terminal, for development) it runs in the
/// foreground until Ctrl+C.
#[cfg(windows)]
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

/// systemd runs the service in the foreground and stops it with SIGTERM.
#[cfg(target_os = "linux")]
pub fn run() -> i32 {
    serve_blocking(super::headless::shutdown_requested()) as i32
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
    // Before anything is written there (the log first).
    if let Err(err) = sys::prepare_data_dir(&data_dir) {
        eprintln!("unsafe data folder {}: {err}", data_dir.display());
        return 3;
    }
    #[cfg(windows)]
    logfile::init(&data_dir);
    // systemd keeps stderr in the journal: `journalctl -u netagg`.
    #[cfg(target_os = "linux")]
    {
        log::set_logger(&super::headless::StderrLog).ok();
        log::set_max_level(log::LevelFilter::Info);
    }
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
#[cfg(windows)]
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
            use std::os::windows::io::AsRawHandle;
            let client = sys::pipe_client(server.as_raw_handle()).and_then(|client| {
                if same_path(&client.image, &own_exe) {
                    Ok(client)
                } else {
                    Err(std::io::Error::other(format!(
                        "refused a request from {}",
                        client.image.display()
                    )))
                }
            });
            if let Err(err) = answer(server, client, &state).await {
                log::warn!("pipe: {err}");
            }
        });
    }
}

/// Answers the control socket until the service stops. Linux checks who
/// connected by user, not by program: a process can hand its connection to
/// another program, and reading other users' programs would take
/// CAP_SYS_PTRACE, which the service doesn't keep.
#[cfg(target_os = "linux")]
async fn listen(state: Arc<AppState>) {
    let listener = match sys::control_listener() {
        Ok(listener) => listener,
        Err(err) => return log::error!("can't serve the control socket: {err}"),
    };
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(err) => {
                log::warn!("control socket: {err}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            let client = sys::pipe_client(&stream);
            if let Err(err) = answer(stream, client, &state).await {
                log::warn!("control socket: {err}");
            }
        });
    }
}

async fn answer(
    stream: impl AsyncRead + AsyncWrite + Unpin,
    client: std::io::Result<sys::PipeClient>,
    state: &AppState,
) -> std::io::Result<()> {
    let client = client?;
    let mut reader = BufReader::new(stream);
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
#[cfg(windows)]
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str().to_string_lossy().to_lowercase() == b.as_os_str().to_string_lossy().to_lowercase()
}

/// Opens the service's end, after checking it's the service's.
#[cfg(windows)]
async fn connect() -> Result<impl AsyncRead + AsyncWrite + Unpin, String> {
    let pipe = loop {
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
    Ok(pipe)
}

#[cfg(target_os = "linux")]
async fn connect() -> Result<impl AsyncRead + AsyncWrite + Unpin, String> {
    let stream = tokio::net::UnixStream::connect(sys::CONTROL_SOCKET)
        .await
        .map_err(|_| UNREACHABLE.to_owned())?;
    // Only root can have made it, so a program can't stand in for the service.
    if !sys::control_socket_trusted().map_err(|e| e.to_string())? {
        return Err(
            "Another program answers on NetAgg's socket instead of the NetAgg service.".into(),
        );
    }
    Ok(stream)
}

/// Sends one request to the service and returns its reply.
pub async fn send(request: &Request) -> Result<Value, String> {
    let exchange = async {
        let mut pipe = connect().await?;
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
#[cfg(feature = "gui")]
pub async fn call(request: Request) -> Result<Value, String> {
    match send(&request).await {
        Err(err) if err == NEEDS_ADMIN && request.needs_admin() => {
            let line = serde_json::to_string(&request).map_err(|e| e.to_string())?;
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            #[cfg(target_os = "linux")]
            let exe = sys::without_deleted(exe);
            let code = tokio::task::spawn_blocking(move || {
                #[cfg(target_os = "linux")]
                return sys::run_elevated(
                    &exe.to_string_lossy(),
                    &format!("{ADMIN_REQUEST_ARG} -"),
                    &hex(line.as_bytes()),
                );
                // An elevated process's command line is hidden from other users.
                #[cfg(not(target_os = "linux"))]
                sys::run_elevated(
                    &exe.to_string_lossy(),
                    &format!("{ADMIN_REQUEST_ARG} {}", hex(line.as_bytes())),
                )
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

/// The installer's step: makes the data folder safe, then takes over the
/// installing user's settings and usage from before the service, if the
/// folder has none. Exit code 0 when the folder is safe to use, 1 otherwise.
#[cfg(windows)]
pub fn run_prepare_data_dir() -> i32 {
    let Some(dir) = sys::service_data_dir().filter(|_| sys::is_elevated()) else {
        return 1;
    };
    if let Err(err) = sys::prepare_data_dir(&dir) {
        eprintln!("{err}");
        return 1;
    }
    if let Some(old) = std::env::var_os("APPDATA").map(|a| Path::new(&a).join(OLD_CONFIG_DIR)) {
        for name in ["settings.json", "usage.json"] {
            take_over(&old.join(name), &dir.join(name));
        }
    }
    0
}

/// The folder versions before the service kept settings in, in AppData.
#[cfg(windows)]
const OLD_CONFIG_DIR: &str = "dev.netagg";

/// Copies `from` to `to` unless `to` exists or `from` isn't a plain file
/// (a link could point the elevated installer anywhere). Written anew, so
/// it gets the data folder's permissions.
#[cfg(windows)]
fn take_over(from: &Path, to: &Path) {
    let plain = std::fs::symlink_metadata(from).is_ok_and(|m| m.file_type().is_file());
    if plain && !to.exists() {
        if let Ok(bytes) = std::fs::read(from) {
            let _ = std::fs::write(to, bytes);
        }
    }
}

/// The elevated copy: sends the one request it was given. Returns the exit
/// code: 0 done, 1 refused or failed, 2 not a request.
pub fn run_admin_request(args: &[String]) -> i32 {
    let Some(request) = args
        .iter()
        .skip_while(|a| *a != ADMIN_REQUEST_ARG)
        .nth(1)
        .and_then(|h| match h.as_str() {
            "-" => read_stdin_line(),
            _ => Some(h.clone()),
        })
        .and_then(|h| unhex(h.trim()))
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

/// One line from standard input, at most `MAX_REQUEST` bytes.
fn read_stdin_line() -> Option<String> {
    use std::io::{BufRead, Read};
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .take(2 * MAX_REQUEST + 2)
        .read_line(&mut line)
        .ok()?;
    Some(line)
}

#[cfg(any(feature = "gui", test))]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
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

    #[cfg(windows)]
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
