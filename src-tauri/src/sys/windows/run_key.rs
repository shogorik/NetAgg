//! Values in the user's Run key: what Windows starts at their sign-in.

use std::io;

use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

use super::elevation::wide;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const ERROR_FILE_NOT_FOUND: u32 = 2;

pub fn has_run_value(name: &str) -> bool {
    let (key, name) = (wide(RUN_KEY), wide(name));
    let code = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    code == 0
}

/// Sets `name` to `command`, or removes it (`None`; gone already is fine).
pub fn set_run_value(name: &str, command: Option<&str>) -> io::Result<()> {
    let (key, name) = (wide(RUN_KEY), wide(name));
    let code = match command {
        Some(command) => {
            let command = wide(command);
            unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    key.as_ptr(),
                    name.as_ptr(),
                    REG_SZ,
                    command.as_ptr().cast(),
                    (command.len() * 2) as u32,
                )
            }
        }
        None => match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) }
        {
            ERROR_FILE_NOT_FOUND => 0,
            code => code,
        },
    };
    match code {
        0 => Ok(()),
        code => Err(io::Error::from_raw_os_error(code as i32)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_reads_and_removes_a_value() {
        let name = format!("NetAggTest{}", std::process::id());
        assert!(!has_run_value(&name));
        set_run_value(&name, Some(r#""C:\x.exe" --autostart"#)).unwrap();
        assert!(has_run_value(&name));
        set_run_value(&name, None).unwrap();
        assert!(!has_run_value(&name));
        set_run_value(&name, None).unwrap();
    }
}
