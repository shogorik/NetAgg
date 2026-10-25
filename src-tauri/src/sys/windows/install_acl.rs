//! NetAgg runs as administrator, and "Start with Windows" starts it that way
//! without asking. If ordinary users can change its folder or files (a
//! hand-made `D:\Program Files` often lets them), anything running as the
//! user could swap in its own code and get those rights. This finds that.

use std::io;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    EqualSid, GetAce, GetTokenInformation, IsWellKnownSid, TokenUser, WinAuthenticatedUserSid,
    WinBuiltinAdministratorsSid, WinBuiltinUsersSid, WinInteractiveSid, WinLocalSystemSid,
    WinWorldSid, ACCESS_ALLOWED_ACE, ACL, DACL_SECURITY_INFORMATION, INHERIT_ONLY_ACE,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::elevation::wide;

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
/// Rights that let someone change what runs: write or append (add a file
/// or folder), delete a child, delete, change permissions or owner, and the
/// generic write and all.
const CHANGE_RIGHTS: u32 = 0x0000_0002
    | 0x0000_0004
    | 0x0000_0040
    | 0x0001_0000
    | 0x0004_0000
    | 0x0008_0000
    | 0x4000_0000
    | 0x1000_0000;

/// What has to be safe: the folder (new DLLs could be planted next to the
/// exe), the exe, and the Wintun DLL with its folder.
fn guarded_paths() -> io::Result<Vec<PathBuf>> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| io::Error::other("NetAgg's folder is unknown"))?
        .to_path_buf();
    let resources = dir.join("resources");
    Ok(vec![
        dir,
        exe,
        resources.clone(),
        resources.join("wintun.dll"),
    ])
}

/// `Err` naming the first file or folder that users who aren't
/// administrators can change. Missing files are skipped.
pub fn check_install_secure() -> io::Result<()> {
    let user = current_user_sid()?;
    for path in guarded_paths()? {
        if path.exists() && changeable_by_users(&path, &user.0)? {
            return Err(io::Error::other(format!(
                "{} can be changed without administrator rights, so other programs could take over NetAgg's \
                 administrator rights. Install NetAgg into Program Files, or let only administrators change \
                 that folder.",
                path.display()
            )));
        }
    }
    Ok(())
}

/// A SID copied out of the token, which is freed with it.
struct OwnedSid(Vec<u8>);

fn current_user_sid() -> io::Result<OwnedSid> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; 256];
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut len,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let sid = user.User.Sid;
        let sid_len = windows_sys::Win32::Security::GetLengthSid(sid) as usize;
        Ok(OwnedSid(
            std::slice::from_raw_parts(sid as *const u8, sid_len).to_vec(),
        ))
    }
}

/// Whoever isn't an administrator: everyone, signed-in and interactive
/// users, the Users group, and this user's own account.
fn is_ordinary(sid: PSID, user: &[u8]) -> bool {
    unsafe {
        [
            WinWorldSid,
            WinAuthenticatedUserSid,
            WinBuiltinUsersSid,
            WinInteractiveSid,
        ]
        .iter()
        .any(|&kind| IsWellKnownSid(sid, kind) != 0)
            || EqualSid(sid, user.as_ptr() as PSID) != 0
    }
}

fn changeable_by_users(path: &Path, user: &[u8]) -> io::Result<bool> {
    let name = wide(&path.to_string_lossy());
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    let code = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    if code != 0 {
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    let result = unsafe {
        // An owner can always rewrite the permissions.
        let owner_ok = IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
            || IsWellKnownSid(owner, WinLocalSystemSid) != 0
            || is_trusted_installer(owner);
        // No DACL at all means everyone has full access.
        !owner_ok || dacl.is_null() || dacl_grants_change(dacl, user)
    };
    unsafe { LocalFree(sd) };
    Ok(result)
}

unsafe fn dacl_grants_change(dacl: *mut ACL, user: &[u8]) -> bool {
    for i in 0..(*dacl).AceCount as u32 {
        let mut ace: *mut core::ffi::c_void = null_mut();
        if GetAce(dacl, i, &mut ace) == 0 {
            continue;
        }
        let ace = &*(ace as *const ACCESS_ALLOWED_ACE);
        // Inherit-only entries are for children, not this object.
        if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE
            || ace.Header.AceFlags as u32 & INHERIT_ONLY_ACE != 0
        {
            continue;
        }
        let sid = &ace.SidStart as *const u32 as PSID;
        if ace.Mask & CHANGE_RIGHTS != 0 && is_ordinary(sid, user) {
            return true;
        }
    }
    false
}

/// `NT SERVICE\TrustedInstaller` (S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464),
/// which owns Windows' own folders.
fn is_trusted_installer(sid: PSID) -> bool {
    const TRUSTED_INSTALLER: [u8; 32] = [
        1, 6, 0, 0, 0, 0, 0, 5, 80, 0, 0, 0, 0xb5, 0x89, 0xfb, 0x38, 0x19, 0x84, 0xc2, 0xcb, 0x5c,
        0x6c, 0x23, 0x6d, 0x57, 0x00, 0x77, 0x6e, 0xc0, 0x02, 0x64, 0x87,
    ];
    unsafe { EqualSid(sid, TRUSTED_INSTALLER.as_ptr() as PSID) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checks an installed copy. Run with
    /// `NETAGG_DIR="D:\Program Files\NetAgg" cargo test installed_acl -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn installed_acl() {
        let user = current_user_sid().unwrap();
        let dir = PathBuf::from(std::env::var("NETAGG_DIR").unwrap());
        for path in [dir.clone(), dir.join("netagg.exe"), dir.join("resources"), dir.join("resources/wintun.dll")] {
            println!("{}: changeable by users = {}", path.display(), changeable_by_users(&path, &user.0).unwrap());
        }
    }

    #[test]
    fn temp_is_unsafe_and_windows_is_safe() {
        let user = current_user_sid().unwrap();
        // The user's own temp folder: they can change it.
        assert!(changeable_by_users(&std::env::temp_dir(), &user.0).unwrap());
        // System32 belongs to TrustedInstaller; users only read it.
        let system32 = PathBuf::from(std::env::var("SystemRoot").unwrap()).join("System32");
        assert!(!changeable_by_users(&system32, &user.0).unwrap());
        assert!(!changeable_by_users(&system32.join("kernel32.dll"), &user.0).unwrap());
    }
}
