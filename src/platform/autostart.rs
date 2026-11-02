//! Per-user Windows autostart registration.
//!
//! A logon task starts tile-rs as soon as the user signs in. The `Run` registry key is the
//! fallback (and what older versions and the installer use): Explorer holds those programs back
//! for several seconds after sign-in, which is exactly the time windows stay untiled.
use crate::ui::tray;
use std::mem::size_of;
use std::os::windows::process::CommandExt;
use std::process::Command;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteKeyValueW, RegGetValueW, RegOpenKeyExW, RegSetKeyValueW,
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ, RRF_RT_REG_SZ,
};

/// The autostart entry lives here (the same place the old `reg.exe` version used).
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "tile-rs";
/// Name of the logon task. Tasks created in the root folder are listed here for every reader.
const TASK_NAME: &str = "tile-rs";
const TASK_TREE: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Schedule\TaskCache\Tree";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Reads the value from `HKCU\<subkey>` without spawning a process.
fn read_value_at(subkey: &str, name: &str) -> Option<String> {
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
    if err != ERROR_SUCCESS {
        return None;
    }
    let chars = (size as usize / size_of::<u16>()).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..chars.min(buf.len())]))
}

fn autostart_enabled_at(subkey: &str, name: &str) -> bool {
    read_value_at(subkey, name).is_some()
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

/// Whether the logon task exists. The task cache is readable by every user, unlike the task files.
fn task_exists() -> bool {
    let key = tray::wide(&format!("{TASK_TREE}{}{TASK_NAME}", char::from(92u8)));
    let mut handle = Default::default();
    let err = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            None,
            KEY_READ,
            &mut handle,
        )
    };
    if err != ERROR_SUCCESS {
        return false;
    }
    unsafe {
        let _ = RegCloseKey(handle);
    }
    true
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The task definition: at this user's sign-in, run `exe` with the user's normal rights, without
/// a time limit and without the battery restrictions tasks have by default.
fn task_xml(exe: &str, user: &str) -> String {
    let (exe, user) = (xml_escape(exe), xml_escape(user));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts tile-rs when you sign in.</Description></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec><Command>{exe}</Command><Arguments>--autostart</Arguments></Exec>
  </Actions>
</Task>
"#
    )
}

fn schtasks(args: &[&str]) -> bool {
    Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Creates (or replaces) the logon task for this executable. False when Windows refuses.
fn create_task(exe: &str) -> bool {
    let (Some(domain), Some(name)) = (std::env::var_os("USERDOMAIN"), std::env::var_os("USERNAME"))
    else {
        return false;
    };
    let user = format!(
        "{}{}{}",
        domain.to_string_lossy(),
        char::from(92u8),
        name.to_string_lossy()
    );
    let path = std::env::temp_dir().join("tile-rs-logon-task.xml");
    // schtasks reads the file as UTF-16 with a byte order mark
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(
        task_xml(exe, &user)
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes()),
    );
    if std::fs::write(&path, bytes).is_err() {
        return false;
    }
    let created = schtasks(&[
        "/Create",
        "/XML",
        &path.to_string_lossy(),
        "/TN",
        TASK_NAME,
        "/F",
    ]);
    let _ = std::fs::remove_file(&path);
    created
}

fn delete_task() -> bool {
    !task_exists() || schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])
}

pub fn autostart_enabled() -> bool {
    task_exists() || autostart_enabled_at(RUN_SUBKEY, RUN_VALUE)
}

pub fn set_autostart(on: bool) -> bool {
    if on {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        if create_task(&exe.to_string_lossy()) {
            // one way of starting is enough; the task is the faster one
            let _ = set_autostart_at(RUN_SUBKEY, RUN_VALUE, None);
            return true;
        }
        let value = format!("\"{}\" --autostart", exe.display());
        set_autostart_at(RUN_SUBKEY, RUN_VALUE, Some(&value))
    } else {
        let task_gone = delete_task();
        let run_gone = !autostart_enabled_at(RUN_SUBKEY, RUN_VALUE)
            || set_autostart_at(RUN_SUBKEY, RUN_VALUE, None);
        task_gone && run_gone
    }
}

/// Moves a `Run` entry that points at this executable (the installer writes one) over to the
/// faster logon task. Runs on a helper thread: starting `schtasks` takes a moment.
pub fn upgrade_to_logon_task() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.to_string_lossy().to_lowercase();
    let on_run_key = read_value_at(RUN_SUBKEY, RUN_VALUE)
        .is_some_and(|value| value.to_lowercase().contains(&exe));
    if !on_run_key || task_exists() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("tile-rs-autostart".into())
        .spawn(|| {
            if set_autostart(true) {
                crate::diagnostics::log("autostart: moved from the Run key to a logon task");
            }
        });
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
        assert_eq!(
            read_value_at(TEST_SUBKEY, TEST_VALUE).as_deref(),
            Some("\"C:\\tile-rs.exe\" --autostart")
        );
        assert!(set_autostart_at(TEST_SUBKEY, TEST_VALUE, None));
        assert!(!autostart_enabled_at(TEST_SUBKEY, TEST_VALUE));
    }

    #[test]
    fn the_logon_task_names_the_user_and_the_program_and_escapes_both() {
        let xml = task_xml(r"C:\Tools\A&B <x>\tile-rs.exe", r"PC\Ann & Bob");
        assert!(xml.contains("<Command>C:\\Tools\\A&amp;B &lt;x&gt;\\tile-rs.exe</Command>"));
        assert!(xml.contains("<Arguments>--autostart</Arguments>"));
        assert_eq!(xml.matches("<UserId>PC\\Ann &amp; Bob</UserId>").count(), 2);
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }
}
