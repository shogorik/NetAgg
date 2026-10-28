//! Turning the WebUI on or off from the Linux desktop app, which carries it
//! switched off. The app runs `pkexec netagg webui-switch` (polkit, as for
//! DNS) and writes what it wants on standard input, so a new password never
//! shows on a command line.

use std::io::{self, BufRead, Read};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::password;

pub const SWITCH_ARG: &str = "webui-switch";
const UNIT: &str = "netagg-webui.service";
/// Room for the longest password (1024 characters, up to 6 bytes each once
/// in JSON); the app sends one short line.
const MAX_REQUEST: u64 = 16 * 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Switch {
    pub on: bool,
    /// A new password, set before the WebUI starts.
    #[serde(default)]
    pub password: Option<String>,
}

/// As root: reads one `Switch` (JSON) from standard input and applies it.
/// Returns the exit code: 2 for no valid request.
pub fn run() -> i32 {
    if unsafe { libc::geteuid() } != 0 {
        eprintln!("Run it as root.");
        return 1;
    }
    let mut line = String::new();
    let Some(switch) = io::stdin()
        .lock()
        .take(MAX_REQUEST)
        .read_line(&mut line)
        .ok()
        .and_then(|_| serde_json::from_str::<Switch>(&line).ok())
    else {
        return 2;
    };
    match apply(&switch) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn apply(switch: &Switch) -> io::Result<()> {
    if let Some(new) = &switch.password {
        password::set(new)?;
        journal("WebUI password changed; open sessions have ended");
    }
    if switch.on && password::stored().is_none() {
        return Err(io::Error::other("Set a WebUI password first."));
    }
    let verb = if switch.on { "enable" } else { "disable" };
    let status = Command::new("systemctl")
        .args([verb, "--now", UNIT])
        .status()?;
    match status.success() {
        true => {
            journal(if switch.on {
                "WebUI switched on"
            } else {
                "WebUI switched off"
            });
            Ok(())
        }
        false => Err(io::Error::other(format!(
            "systemctl {verb} {UNIT}: {status}"
        ))),
    }
}

/// Run through pkexec or sudo, this process has no journal of its own: its
/// output goes to whoever started it. `logger` (util-linux) puts it next to
/// the service's.
pub(super) fn journal(message: &str) {
    let by = std::env::var("PKEXEC_UID")
        .or_else(|_| std::env::var("SUDO_UID"))
        .map_or(String::new(), |uid| format!(" (by uid {uid})"));
    let _ = Command::new("logger")
        .args(["-t", super::CLI, &format!("{message}{by}")])
        .status();
}

/// What the desktop app shows: whether the WebUI runs, and whether it has a
/// password yet. Needs no rights.
#[cfg(feature = "gui")]
pub fn state() -> (bool, bool) {
    let on = Command::new("systemctl")
        .args(["is-active", "--quiet", UNIT])
        .status()
        .is_ok_and(|s| s.success());
    let has_password = std::path::Path::new(password::PASSWORD_FILE).exists();
    (on, has_password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_request_the_app_sends() {
        let on: Switch = serde_json::from_str(r#"{"on":true,"password":"long enough"}"#).unwrap();
        assert_eq!(
            on,
            Switch {
                on: true,
                password: Some("long enough".into())
            }
        );
        let off: Switch = serde_json::from_str(r#"{"on":false}"#).unwrap();
        assert_eq!(off.password, None);
    }
}
