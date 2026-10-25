//! The service's data folder, `%ProgramData%\NetAgg`. Ordinary users may
//! create folders in ProgramData, so one could be there first: a junction
//! that sends the installer's and the service's writes (and permission
//! changes) elsewhere, or a folder of their own with their own settings.
//! Both the installer (`--prepare-data-dir`) and the service at start make
//! sure it's a real folder only administrators and SYSTEM can change.

use std::io;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;

use super::elevation::wide;
use super::install_acl::{changeable_by_users, SYSTEM_SID};
use super::instance::Security;

/// Owned by Administrators; SYSTEM and Administrators have full control,
/// Users read. Protected: nothing inherited from ProgramData.
const DATA_DIR_SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FRFX;;;BU)";

/// Leaves `dir` a real folder that only administrators and SYSTEM can
/// change: a link there is removed (not what it points to), and a folder
/// someone else could have changed is moved aside, then it's created anew.
pub fn prepare_data_dir(dir: &Path) -> io::Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            // A junction or directory link: removing it leaves its target alone.
            std::fs::remove_dir(dir).or_else(|_| std::fs::remove_file(dir))?;
        } else if !meta.is_dir() || !trusted(dir)? {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let name = format!(
                "{}.untrusted-{secs}",
                dir.file_name().unwrap_or_default().to_string_lossy()
            );
            std::fs::rename(dir, dir.with_file_name(name))?;
        }
    }
    if !dir.exists() {
        create_protected(dir)?;
    }
    Ok(())
}

/// Nobody but administrators and SYSTEM could have changed the folder or
/// anything in it, and it holds only plain files.
fn trusted(dir: &Path) -> io::Result<bool> {
    if changeable_by_users(dir, &SYSTEM_SID)? {
        return Ok(false);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        // Not followed: a link is reported as one.
        let kind = entry.file_type()?;
        if kind.is_symlink() || kind.is_dir() || changeable_by_users(&entry.path(), &SYSTEM_SID)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Creates the folder with its permissions from the start, so there's no
/// moment when it has ProgramData's.
fn create_protected(dir: &Path) -> io::Result<()> {
    let security = Security::new(DATA_DIR_SDDL)?;
    let path = wide(&dir.to_string_lossy());
    if unsafe { CreateDirectoryW(path.as_ptr(), &security.attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("netagg-datadir-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn a_junction_is_removed_not_followed() {
        let root = scratch("junction");
        let target = root.join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("keep.txt"), "x").unwrap();
        let link = root.join("NetAgg");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success());
        // Creating a protected folder as an ordinary user may be refused
        // (owner Administrators); the link must be gone either way.
        let _ = prepare_data_dir(&link);
        assert!(!std::fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()));
        assert!(target.join("keep.txt").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_folder_users_can_change_is_moved_aside() {
        let root = scratch("aside");
        let dir = root.join("NetAgg");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), "{}").unwrap();
        let _ = prepare_data_dir(&dir);
        let names: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.iter().any(|n| n.starts_with("NetAgg.untrusted-")),
            "{names:?}"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// As the installer and the service run it: elevated. Run from an
    /// elevated terminal: `cargo test elevated_ -- --ignored`.
    #[test]
    #[ignore]
    fn elevated_creates_a_folder_only_administrators_change() {
        let root = scratch("elevated");
        let target = root.join("target");
        std::fs::create_dir(&target).unwrap();
        let dir = root.join("NetAgg");
        let made = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&dir)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success());
        prepare_data_dir(&dir).unwrap();
        let meta = std::fs::symlink_metadata(&dir).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink());
        assert!(!changeable_by_users(&dir, &SYSTEM_SID).unwrap());
        // A file written there gets the folder's permissions.
        std::fs::write(dir.join("settings.json"), "{}").unwrap();
        assert!(!changeable_by_users(&dir.join("settings.json"), &SYSTEM_SID).unwrap());
        assert!(trusted(&dir).unwrap());
        // Trusted as it is: a second run changes nothing.
        prepare_data_dir(&dir).unwrap();
        assert!(dir.join("settings.json").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_permissions_parse() {
        Security::new(DATA_DIR_SDDL).unwrap();
    }
}
