//! Windows Connection Manager's "Minimize the number of simultaneous
//! connections" policy. By default Windows prefers Ethernet over Wi-Fi: with
//! an Ethernet connection up it blocks automatic Wi-Fi connections and drops
//! the one it has. A USB-tethered phone shows up as Ethernet, so Wi-Fi goes
//! away exactly when there are two networks to combine. Value 0 allows
//! simultaneous connections.

use std::io;

use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD};

use super::elevation::{run_elevated, wide};

const KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows\WcmSvc\GroupPolicy";
const VALUE: &str = "fMinimizeConnections";

/// True unless the policy is set to allow simultaneous connections; not
/// configured means Windows' default, which minimizes them.
pub fn minimizes_connections() -> bool {
    let (key, value) = (wide(KEY), wide(VALUE));
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let code = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut data as *mut u32).cast(),
            &mut size,
        )
    };
    code != 0 || data != 0
}

/// Sets the policy to 0, through the UAC prompt unless NetAgg already has
/// administrator rights. Windows applies it at the next sign-in.
pub fn allow_simultaneous_connections() -> io::Result<()> {
    let args = format!(r#"add "HKLM\{KEY}" /v {VALUE} /t REG_DWORD /d 0 /f"#);
    match run_elevated("reg", &args)? {
        0 => Ok(()),
        code => Err(io::Error::other(format!("reg failed with code {code}"))),
    }
}
