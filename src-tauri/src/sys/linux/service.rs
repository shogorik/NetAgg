//! The Linux side of the NetAgg service (`netagg.service`, root): where it
//! keeps its data, its control socket and who may use it, the check that
//! its files can only be changed by root, and the polkit step that runs one
//! request as root (the Windows UAC prompt).

use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::{UnixListener, UnixStream};

/// The service's control socket, in its `RuntimeDirectory=`.
pub const CONTROL_SOCKET: &str = "/run/netagg/control.sock";
/// The WebUI (netagg-server) runs as this system user; being logged in to it
/// counts as administrator.
pub const WEBUI_USER: &str = "netagg-web";
/// Users below this id are system accounts; ordinary people start here
/// (Debian and Ubuntu's default `UID_MIN`).
const DEFAULT_UID_MIN: u32 = 1000;
const NOBODY: u32 = 65534;
/// systemd's dynamic service users (`DynamicUser=`): services, not people.
const DYNAMIC_USERS: std::ops::RangeInclusive<u32> = 61184..=65519;

/// Settings, usage and the log: the unit's `StateDirectory=`.
pub fn service_data_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("STATE_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/netagg"));
    Some(dir)
}

/// Leaves `dir` a real folder only root can change, holding only files only
/// root can change. systemd creates it so; anything else means someone with
/// root rights changed it, and the service won't trust it.
pub fn prepare_data_dir(dir: &Path) -> io::Result<()> {
    if std::fs::symlink_metadata(dir).is_err() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || !root_only(&meta) {
        return Err(untrusted(dir));
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        if !meta.is_file() || !root_only(&meta) {
            return Err(untrusted(&entry.path()));
        }
    }
    Ok(())
}

fn untrusted(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{} isn't a plain file or folder that only root can change",
            path.display()
        ),
    )
}

/// Owned by root and not writable by its group or anyone else.
fn root_only(meta: &std::fs::Metadata) -> bool {
    meta.uid() == 0 && meta.mode() & 0o022 == 0
}

/// Refuses to run as root from a program that others could replace: the
/// executable and every folder above it must be root's alone.
pub fn check_install_secure() -> io::Result<()> {
    let exe = std::fs::canonicalize(std::env::current_exe()?)?;
    for path in exe.ancestors() {
        let meta = std::fs::metadata(path)?;
        if !root_only(&meta) {
            return Err(io::Error::other(format!(
                "{} can be changed by users other than root, so NetAgg won't run from it. Reinstall NetAgg.",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Starts serving the control socket: any local user may connect (the
/// service checks who did), only root can have created it.
pub fn control_listener() -> io::Result<UnixListener> {
    let path = Path::new(CONTROL_SOCKET);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A socket left by a service that crashed; the folder is root's.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
    Ok(listener)
}

/// Who is on the other end of the control socket.
pub struct PipeClient {
    /// Root (the polkit-approved copy), or the WebUI.
    pub elevated: bool,
}

/// The connected process, if it's a user NetAgg answers: root, a person
/// (not a system account), or the WebUI.
pub fn pipe_client(stream: &UnixStream) -> io::Result<PipeClient> {
    let cred = stream.peer_cred()?;
    let webui = user_id(WEBUI_USER);
    let uid = cred.uid();
    let elevated = uid == 0 || Some(uid) == webui;
    if !elevated && !is_person(uid, uid_min()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("user {uid} isn't one NetAgg answers"),
        ));
    }
    Ok(PipeClient { elevated })
}

fn is_person(uid: u32, uid_min: u32) -> bool {
    uid >= uid_min && uid != NOBODY && !DYNAMIC_USERS.contains(&uid)
}

/// An executable replaced by an update reads as `… (deleted)` until the
/// process restarts; it's still the same program.
#[cfg(any(feature = "gui", test))]
pub fn without_deleted(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        Some(p) => PathBuf::from(p),
        None => path,
    }
}

fn uid_min() -> u32 {
    std::fs::read_to_string("/etc/login.defs")
        .ok()
        .and_then(|defs| parse_uid_min(&defs))
        .unwrap_or(DEFAULT_UID_MIN)
}

fn parse_uid_min(defs: &str) -> Option<u32> {
    defs.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("UID_MIN"))
        .and_then(|rest| rest.trim().parse().ok())
}

/// A user's id, through the system's user database (`/etc/passwd`, and
/// systemd's dynamic users).
fn user_id(name: &str) -> Option<u32> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let mut buf = vec![0 as libc::c_char; 4096];
    let code = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &mut entry,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        )
    };
    (code == 0 && !found.is_null()).then_some(entry.pw_uid)
}

/// The client's check that the socket is the service's: its folder belongs
/// to root and nobody else may put anything there.
pub fn control_socket_trusted() -> io::Result<bool> {
    let socket = Path::new(CONTROL_SOCKET);
    let dir = socket.parent().unwrap_or(Path::new("/"));
    let dir_meta = std::fs::symlink_metadata(dir)?;
    let meta = std::fs::symlink_metadata(socket)?;
    Ok(dir_meta.is_dir() && root_only(&dir_meta) && meta.uid() == 0)
}

/// Runs `program args` as root after the polkit password dialog and waits
/// for it. An error means it never ran: the dialog was dismissed, or the
/// user may not approve it.
#[cfg(feature = "gui")]
pub fn run_elevated(program: &str, args: &str) -> io::Result<u32> {
    let status = std::process::Command::new("pkexec")
        .arg(program)
        .args(args.split_whitespace())
        .status()
        .map_err(|err| io::Error::new(err.kind(), format!("couldn't run pkexec: {err}")))?;
    match status.code() {
        // pkexec's own: not authorised or dismissed (126), or no such program (127).
        Some(126 | 127) | None => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the administrator approval wasn't given",
        )),
        Some(code) => Ok(code as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_are_answered_system_accounts_are_not() {
        assert!(is_person(1000, 1000));
        assert!(is_person(1002, 1000));
        assert!(!is_person(33, 1000)); // www-data
        assert!(!is_person(999, 1000));
        assert!(!is_person(NOBODY, 1000));
        assert!(!is_person(61184, 1000));
    }

    #[test]
    fn reads_uid_min() {
        let defs = "# comment\nUID_MAX\t\t\t60000\nUID_MIN\t\t\t 1000\n#UID_MIN 5\n";
        assert_eq!(parse_uid_min(defs), Some(1000));
        assert_eq!(parse_uid_min("SYS_UID_MIN 100\n"), None);
    }

    #[test]
    fn finds_a_user() {
        assert_eq!(user_id("root"), Some(0));
        assert_eq!(user_id("no-such-user-netagg"), None);
    }

    #[test]
    fn an_updated_program_is_still_itself() {
        assert_eq!(
            without_deleted(PathBuf::from("/usr/bin/netagg (deleted)")),
            PathBuf::from("/usr/bin/netagg")
        );
        assert_eq!(
            without_deleted(PathBuf::from("/usr/bin/netagg")),
            PathBuf::from("/usr/bin/netagg")
        );
    }

    #[test]
    fn a_folder_others_can_write_is_refused() {
        let dir = std::env::temp_dir().join(format!("netagg-datadir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Not root's, so never trusted in a test run as a user; as root, a
        // world-writable mode is what refuses it.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(prepare_data_dir(&dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
