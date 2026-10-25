//! Start NetAgg when the user signs in, quietly in the tray.

#[cfg(target_os = "linux")]
pub use desktop_entry::{is_enabled, set_enabled};
#[cfg(target_os = "macos")]
pub use launch_agent::{is_enabled, set_enabled};
#[cfg(windows)]
pub use run_key::{is_enabled, set_enabled};

/// Tells NetAgg it was started at sign-in, so it stays in the tray.
pub const AUTOSTART_ARG: &str = "--autostart";

/// The tray app runs as the user (the service has the rights), so the
/// user's own Run key starts it, with no prompt.
#[cfg(windows)]
mod run_key {
    use std::io;

    use super::AUTOSTART_ARG;
    use crate::sys;

    const VALUE: &str = "NetAgg";

    pub fn is_enabled() -> bool {
        sys::has_run_value(VALUE)
    }

    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let exe = std::env::current_exe()?;
        let command = command_line(&exe.to_string_lossy());
        sys::set_run_value(VALUE, enabled.then_some(command.as_str()))
    }

    fn command_line(exe: &str) -> String {
        format!("\"{exe}\" {AUTOSTART_ARG}")
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn quotes_the_path() {
            assert_eq!(
                super::command_line(r"D:\Program Files\NetAgg\netagg.exe"),
                r#""D:\Program Files\NetAgg\netagg.exe" --autostart"#
            );
        }
    }
}

/// Desktop sessions start whatever has an entry in the autostart folder. The
/// network permissions live on the executable itself, so no prompt is needed.
#[cfg(target_os = "linux")]
mod desktop_entry {
    use std::io;
    use std::path::PathBuf;

    use super::AUTOSTART_ARG;

    pub fn is_enabled() -> bool {
        entry_path().is_some_and(|path| path.exists())
    }

    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let path =
            entry_path().ok_or_else(|| io::Error::other("could not find the home folder"))?;
        if enabled {
            let exe = std::env::current_exe()?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, entry(&exe.to_string_lossy()))
        } else {
            match std::fs::remove_file(&path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }

    fn entry_path() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(config.join("autostart").join("netagg.desktop"))
    }

    /// The executable is quoted as the desktop entry spec asks, with `%`
    /// doubled so it isn't read as a field code.
    fn entry(exe: &str) -> String {
        let mut quoted = String::new();
        for c in exe.chars() {
            match c {
                '"' | '`' | '$' | '\\' => {
                    quoted.push('\\');
                    quoted.push(c);
                }
                '%' => quoted.push_str("%%"),
                c => quoted.push(c),
            }
        }
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=NetAgg\n\
             Comment=Starts NetAgg when you sign in.\n\
             Exec=\"{quoted}\" {AUTOSTART_ARG}\n\
             Icon=netagg\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn entry_runs_the_app_at_sign_in() {
            let text = entry("/opt/Multi Path/50%/multi\"path");
            assert!(text.starts_with("[Desktop Entry]\nType=Application\n"));
            assert!(text.contains("Exec=\"/opt/Multi Path/50%%/multi\\\"path\" --autostart\n"));
        }
    }
}

/// launchd starts every agent in `~/Library/LaunchAgents` when the user logs
/// in. NetAgg asks for administrator rights as it turns on, so the agent
/// itself needs none.
#[cfg(target_os = "macos")]
mod launch_agent {
    use std::io;
    use std::path::PathBuf;

    use super::AUTOSTART_ARG;

    const LABEL: &str = "dev.netagg";

    pub fn is_enabled() -> bool {
        agent_path().is_some_and(|path| path.exists())
    }

    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let path =
            agent_path().ok_or_else(|| io::Error::other("could not find the home folder"))?;
        if enabled {
            let exe = std::env::current_exe()?;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, plist(&exe.to_string_lossy()))
        } else {
            match std::fs::remove_file(&path) {
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }

    fn agent_path() -> Option<PathBuf> {
        let home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
        Some(
            PathBuf::from(home)
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
        )
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    /// Runs once at login, in the user's session, and isn't restarted after Quit.
    fn plist(exe: &str) -> String {
        let exe = escape(exe);
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>{AUTOSTART_ARG}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <false/>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
</dict>
</plist>
"#
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn agent_runs_the_app_at_login() {
            let text = plist("/Applications/A&B <x>.app/Contents/MacOS/netagg");
            assert!(text.contains(
                "<string>/Applications/A&amp;B &lt;x&gt;.app/Contents/MacOS/netagg</string>"
            ));
            assert!(text.contains("<string>--autostart</string>"));
            assert!(text.contains("<key>RunAtLoad</key>\n  <true/>"));
            assert!(text.contains("<string>dev.netagg</string>"));
        }

        #[test]
        fn agent_lives_in_the_users_library() {
            let path = agent_path().unwrap();
            assert!(path.ends_with("Library/LaunchAgents/dev.netagg.plist"));
        }
    }
}
