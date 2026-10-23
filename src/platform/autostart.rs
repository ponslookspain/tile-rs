//! Per-user Windows autostart registration.
use crate::ui::tray;
use std::mem::size_of;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

/// The autostart entry lives here (the same place the old `reg.exe` version used).
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "tile-rs";

/// Reads the value from `HKCU\<subkey>` without spawning a process.
fn autostart_enabled_at(subkey: &str, name: &str) -> bool {
    let sub = tray::wide(subkey);
    let val = tray::wide(name);
    let mut buf = [0u16; 2048];
    let mut size = (buf.len() * size_of::<u16>()) as u32;
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            PCWSTR(val.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        )
    };
    err == ERROR_SUCCESS
}

/// Writes (`Some`) or removes (`None`) the value in `HKCU\<subkey>`.
fn set_autostart_at(subkey: &str, name: &str, value: Option<&str>) -> bool {
    let sub = tray::wide(subkey);
    let val = tray::wide(name);
    match value {
        Some(v) => {
            let data: Vec<u16> = v.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    data.as_ptr() as *const u8,
                    data.len() * size_of::<u16>(),
                )
            };
            let err = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sub.as_ptr()),
                    PCWSTR(val.as_ptr()),
                    REG_SZ.0,
                    Some(bytes.as_ptr() as *const _),
                    bytes.len() as u32,
                )
            };
            err == ERROR_SUCCESS
        }
        None => {
            let err = unsafe {
                RegDeleteKeyValueW(
                    HKEY_CURRENT_USER,
                    PCWSTR(sub.as_ptr()),
                    PCWSTR(val.as_ptr()),
                )
            };
            err == ERROR_SUCCESS || err == ERROR_FILE_NOT_FOUND
        }
    }
}

pub fn autostart_enabled() -> bool {
    autostart_enabled_at(RUN_SUBKEY, RUN_VALUE)
}

pub fn set_autostart(on: bool) -> bool {
    if on {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let value = format!("\"{}\" --autostart", exe.display());
        set_autostart_at(RUN_SUBKEY, RUN_VALUE, Some(&value))
    } else {
        !autostart_enabled() || set_autostart_at(RUN_SUBKEY, RUN_VALUE, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TEST_SUBKEY: &str = r"Software\tile-rs-test";
    const TEST_VALUE: &str = "tile-rs-test";

    #[test]
    fn autostart_registry_roundtrip() {
        let _ = set_autostart_at(TEST_SUBKEY, TEST_VALUE, None); // start clean
        assert!(!autostart_enabled_at(TEST_SUBKEY, TEST_VALUE));
        assert!(set_autostart_at(
            TEST_SUBKEY,
            TEST_VALUE,
            Some("\"C:\\tile-rs.exe\" --autostart")
        ));
        assert!(autostart_enabled_at(TEST_SUBKEY, TEST_VALUE));
        assert!(set_autostart_at(TEST_SUBKEY, TEST_VALUE, None));
        assert!(!autostart_enabled_at(TEST_SUBKEY, TEST_VALUE));
    }
}
