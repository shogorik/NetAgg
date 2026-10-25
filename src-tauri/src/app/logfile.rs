//! The app's own log file, since the windowed app has no console:
//! `netagg.log` in its local data folder. Each line names the process, as
//! turning on can relaunch NetAgg with administrator rights.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Started again once it grows past this.
const MAX_BYTES: u64 = 512 * 1024;

struct FileLog {
    file: Mutex<File>,
    pid: u32,
}

impl log::Log for FileLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target().starts_with("netagg_lib") && metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let _ = writeln!(
                self.file.lock().unwrap(),
                "{secs} [{}] {:5} {}",
                self.pid,
                record.level(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

/// Logs to `dir/netagg.log` from now on. Errors are ignored: no log is no
/// reason not to run.
pub fn init(dir: &Path) {
    let _ = std::fs::create_dir_all(dir);
    let path = dir.join("netagg.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = std::fs::remove_file(&path);
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let logger = FileLog {
        file: Mutex::new(file),
        pid: std::process::id(),
    };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
