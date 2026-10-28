//! The WebUI password. It's never stored: only its argon2id hash (salted),
//! in a file root owns and the WebUI's user may read. Forgetting it is
//! solved by setting a new one (`sudo netagg-server webui-password`), which
//! also ends every open session.

use std::io::{self, BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;

pub const PASSWORD_FILE: &str = "/etc/netagg/webui-password";
pub const PASSWORD_ARG: &str = "webui-password";
/// Reads the new password from standard input instead of the terminal.
const STDIN_ARG: &str = "--stdin";
const MIN_LEN: usize = 8;
const MAX_LEN: usize = 1024;

pub fn hash(password: &str) -> io::Result<String> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|err| io::Error::other(format!("couldn't hash the password: {err}")))
}

/// Takes a deliberate fraction of a second: guessing is slow.
pub fn verify(password: &str, hash: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

/// The stored hash; `None` while no password has been set.
pub fn stored() -> Option<String> {
    let hash = std::fs::read_to_string(PASSWORD_FILE)
        .ok()?
        .trim()
        .to_owned();
    (!hash.is_empty()).then_some(hash)
}

/// `sudo netagg-server webui-password`: asks for the new password twice
/// and stores its hash. Returns the exit code.
pub fn run(args: &[String]) -> i32 {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("Run it as root: sudo {} {PASSWORD_ARG}", super::CLI);
        return 1;
    }
    let password = if args.iter().any(|a| a == STDIN_ARG) {
        read_line(&mut io::stdin().lock())
    } else {
        read_twice()
    };
    match password.and_then(|password| set(&password)) {
        Ok(()) => {
            super::switch::journal("WebUI password changed; open sessions have ended");
            println!("WebUI password set. Open sessions have ended; sign in again.");
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

/// Stores a new password's hash, which ends every open session. Needs root.
pub fn set(password: &str) -> io::Result<()> {
    check_length(password)?;
    save(Path::new(PASSWORD_FILE), &hash(password)?)
}

fn check_length(password: &str) -> io::Result<()> {
    match password.chars().count() {
        n if n < MIN_LEN => Err(io::Error::other(format!(
            "Use at least {MIN_LEN} characters."
        ))),
        n if n > MAX_LEN => Err(io::Error::other("That's too long.")),
        _ => Ok(()),
    }
}

fn read_twice() -> io::Result<String> {
    let first = prompt("New WebUI password: ")?;
    let second = prompt("Again: ")?;
    if first != second {
        return Err(io::Error::other(
            "The two didn't match; nothing was changed.",
        ));
    }
    Ok(first)
}

/// Reads a line from the terminal without showing it.
fn prompt(label: &str) -> io::Result<String> {
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&tty);
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut term) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let shown = term;
    term.c_lflag &= !libc::ECHO;
    term.c_lflag |= libc::ECHONL;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    let mut out = &tty;
    let line = out
        .write_all(label.as_bytes())
        .and_then(|()| out.flush())
        .and_then(|()| read_line(&mut io::BufReader::new(&tty)));
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &shown) };
    line
}

fn read_line(input: &mut impl BufRead) -> io::Result<String> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// Writes the hash where only root can change it and the WebUI's group can
/// read it, replacing the old one in one step.
fn save(path: &Path, hash: &str) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    let gid = group_id(crate::sys::WEBUI_USER).unwrap_or(0);
    let tmp = path.with_extension("new");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o640))?;
        std::os::unix::fs::fchown(&file, Some(0), Some(gid))?;
        writeln!(file, "{hash}")?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Through the system's group database (`/etc/group`, and systemd's dynamic
/// users' groups).
fn group_id(name: &str) -> Option<u32> {
    let name = std::ffi::CString::new(name).ok()?;
    let mut entry: libc::group = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::group = std::ptr::null_mut();
    let mut buf = vec![0 as libc::c_char; 4096];
    let code = unsafe {
        libc::getgrnam_r(
            name.as_ptr(),
            &mut entry,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        )
    };
    (code == 0 && !found.is_null()).then_some(entry.gr_gid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_a_salted_argon2id_hash_never_the_password() {
        let password = "correct horse battery";
        let a = hash(password).unwrap();
        let b = hash(password).unwrap();
        assert!(a.starts_with("$argon2id$"), "{a}");
        assert!(!a.contains(password));
        // A new salt each time.
        assert_ne!(a, b);
        assert!(verify(password, &a) && verify(password, &b));
        assert!(!verify("correct horse battery!", &a));
        assert!(!verify(password, "not a hash"));
    }

    #[test]
    fn needs_a_reasonable_length() {
        assert!(check_length("short").is_err());
        assert!(check_length("long enough").is_ok());
        assert!(check_length(&"x".repeat(MAX_LEN + 1)).is_err());
    }

    #[test]
    fn reads_one_line() {
        let mut input = io::Cursor::new("secret pass\r\nrest\n");
        assert_eq!(read_line(&mut input).unwrap(), "secret pass");
    }
}
