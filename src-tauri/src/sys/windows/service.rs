//! The NetAgg Windows service: the elevated half of NetAgg, without a
//! window. The installer registers it; the tray app, running as the user,
//! talks to it through a named pipe.

use std::ffi::c_void;
use std::io;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_CALL_NOT_IMPLEMENTED, ERROR_SERVICE_SPECIFIC_ERROR, HANDLE,
    NO_ERROR,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT};
use windows_sys::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, TokenElevation, WinBuiltinAdministratorsSid,
    WinLocalSystemSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_ELEVATION,
    TOKEN_QUERY,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_STATUS,
    SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::elevation::wide;
use super::instance::Security;

/// The name the installer registers the service under.
pub const SERVICE_NAME: &str = "NetAgg";
pub const PIPE_NAME: &str = r"\\.\pipe\NetAgg";
/// The service and administrators may do anything with the pipe;
/// interactive users (the tray app) may read and write it. Nobody else
/// connects, and low integrity processes (sandboxed browser tabs) can't write
/// to it (no label means medium, no write up).
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

/// Where the service keeps its settings, usage and log. The installer lets
/// only administrators and the service change it.
pub fn service_data_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("ProgramData")?).join("NetAgg"))
}

/// A new instance of the service's pipe, open as `PIPE_SDDL` says, to
/// local clients only. `first` fails if another process holds the name.
pub fn create_pipe(first: bool) -> io::Result<NamedPipeServer> {
    let mut security = Security::new(PIPE_SDDL)?;
    let attributes: *mut c_void = std::ptr::addr_of_mut!(security.attributes).cast();
    // The attributes are only read while creating.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(PIPE_NAME, attributes)
    }
}

/// Who connected to the service's end of a pipe.
pub struct PipeClient {
    pub image: PathBuf,
    /// The process runs with administrator rights (UAC elevated, or SYSTEM).
    pub elevated: bool,
}

pub fn pipe_client(pipe: HANDLE) -> io::Result<PipeClient> {
    unsafe {
        let mut pid = 0u32;
        if GetNamedPipeClientProcessId(pipe, &mut pid) == 0 {
            return Err(io::Error::last_os_error());
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        let result = client_of(process);
        CloseHandle(process);
        result
    }
}

/// Whether the pipe this client end is connected to belongs to SYSTEM or
/// Administrators: created by the service (or, in development, an elevated
/// terminal), not by a program running as the user, which can't give its
/// objects either owner. (A user can't open the service's process to ask.)
pub fn pipe_owned_by_admins(pipe: HANDLE) -> io::Result<bool> {
    let mut owner: PSID = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let code = unsafe {
        GetSecurityInfo(
            pipe,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut sd,
        )
    };
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let trusted = unsafe {
        IsWellKnownSid(owner, WinLocalSystemSid) != 0
            || IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
    };
    unsafe { LocalFree(sd) };
    Ok(trusted)
}

unsafe fn client_of(process: HANDLE) -> io::Result<PipeClient> {
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    if QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len) == 0 {
        return Err(io::Error::last_os_error());
    }
    let image = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
    let mut token: HANDLE = null_mut();
    if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut size = 0u32;
    let ok = GetTokenInformation(
        token,
        TokenElevation,
        (&mut elevation as *mut TOKEN_ELEVATION).cast(),
        std::mem::size_of::<TOKEN_ELEVATION>() as u32,
        &mut size,
    );
    CloseHandle(token);
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PipeClient {
        image,
        elevated: elevation.TokenIsElevated != 0,
    })
}

type Body = Box<dyn Fn(Receiver<()>) -> u32 + Send + Sync>;

static BODY: OnceLock<Body> = OnceLock::new();
static STATUS_HANDLE: AtomicUsize = AtomicUsize::new(0);
static STOP: Mutex<Option<Sender<()>>> = Mutex::new(None);

/// Runs `body` as the service, until it returns: it gets a message when
/// Windows asks the service to stop, and returns the exit code. Fails with
/// `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` (1063) when not started by
/// the service manager.
pub fn run_service(body: impl Fn(Receiver<()>) -> u32 + Send + Sync + 'static) -> io::Result<()> {
    let _ = BODY.set(Box::new(body));
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
    ];
    // Returns once the service has stopped.
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut windows_sys::core::PWSTR) {
    let (stop, stopped) = channel();
    *STOP.lock().unwrap() = Some(stop);
    let name = wide(SERVICE_NAME);
    let handle = RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(on_control), null_mut());
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle as usize, Ordering::Relaxed);
    report(SERVICE_RUNNING, 0);
    let code = BODY.get().map_or(1, |body| body(stopped));
    report(SERVICE_STOPPED, code);
}

unsafe extern "system" fn on_control(
    control: u32,
    _event: u32,
    _data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING, 0);
            if let Some(stop) = STOP.lock().unwrap().take() {
                let _ = stop.send(());
            }
            NO_ERROR
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR,
        _ => ERROR_CALL_NOT_IMPLEMENTED,
    }
}

fn report(state: u32, code: u32) {
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        },
        dwWin32ExitCode: if code == 0 {
            NO_ERROR
        } else {
            ERROR_SERVICE_SPECIFIC_ERROR
        },
        dwServiceSpecificExitCode: code,
        dwCheckPoint: 0,
        // Turning off takes up to the engine's stop timeout.
        dwWaitHint: if state == SERVICE_STOP_PENDING {
            15_000
        } else {
            0
        },
    };
    let handle = STATUS_HANDLE.load(Ordering::Relaxed);
    unsafe { SetServiceStatus(handle as _, &status) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_security_parses() {
        Security::new(PIPE_SDDL).unwrap();
    }

    #[test]
    fn outside_the_service_manager_it_says_so() {
        let err = run_service(|_| 0).unwrap_err();
        // ERROR_FAILED_SERVICE_CONTROLLER_CONNECT
        assert_eq!(err.raw_os_error(), Some(1063));
    }
}
