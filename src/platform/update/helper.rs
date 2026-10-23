//! Native update helper, executed from a temporary copy before normal manager startup.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, INFINITE,
    PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::Shell::{
    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

#[derive(Debug)]
struct Request {
    parent: u32,
    exe: PathBuf,
    target: PathBuf,
    setup: PathBuf,
    ready: PathBuf,
    autostart: bool,
}

impl Request {
    fn parse(mut value: impl FnMut(&str) -> Option<OsString>) -> Result<Self, String> {
        let mut required = |name| {
            value(name)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("Missing update helper input: {name}"))
        };
        let parent = required("TILE_UPDATE_PARENT")?
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|pid| *pid != 0 && *pid != std::process::id())
            .ok_or("Invalid update parent process")?;
        let request = Self {
            parent,
            exe: required("TILE_UPDATE_EXE")?.into(),
            target: required("TILE_UPDATE_TARGET")?.into(),
            setup: required("TILE_UPDATE_SETUP")?.into(),
            ready: required("TILE_UPDATE_READY")?.into(),
            autostart: match required("TILE_UPDATE_AUTOSTART")?.to_str() {
                Some("1") => true,
                Some("0") => false,
                _ => return Err("Invalid update autostart choice".into()),
            },
        };
        if [
            &request.exe,
            &request.target,
            &request.setup,
            &request.ready,
        ]
        .iter()
        .any(|path| !path.is_absolute())
        {
            return Err("Update helper paths must be absolute".into());
        }
        Ok(request)
    }

    fn validate(&self) -> Result<(), String> {
        let canonical = |path: &Path| {
            path.canonicalize()
                .map_err(|e| format!("Cannot validate update path: {e}"))
        };
        let helper = canonical(&std::env::current_exe().map_err(|e| e.to_string())?)?;
        let stage = helper.parent().ok_or("Invalid update helper folder")?;
        let setup = canonical(&self.setup)?;
        let ready_parent = canonical(self.ready.parent().ok_or("Invalid ready-file path")?)?;
        let exe = canonical(&self.exe)?;
        let target = canonical(&self.target)?;
        if helper.file_name() != Some(OsStr::new("apply-update.exe"))
            || setup.file_name() != Some(OsStr::new("setup.exe"))
            || self.ready.file_name() != Some(OsStr::new("ready"))
            || setup.parent() != Some(stage)
            || ready_parent != stage
            || exe.parent() != Some(target.as_path())
            || exe == helper
            || self.ready.exists()
        {
            return Err("Invalid update staging paths. Nothing was installed.".into());
        }
        Ok(())
    }
}

struct Process(HANDLE);

impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open_process(pid: u32) -> windows::core::Result<Process> {
    unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    }
    .map(Process)
}

fn process_image(process: &Process) -> Result<PathBuf, String> {
    let mut path = vec![0u16; 32768];
    let mut length = path.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            process.0,
            PROCESS_NAME_FORMAT(0),
            PWSTR(path.as_mut_ptr()),
            &mut length,
        )
    }
    .map_err(|e| format!("Cannot identify an update process: {e}"))?;
    PathBuf::from(OsString::from_wide(&path[..length as usize]))
        .canonicalize()
        .map_err(|e| format!("Cannot identify the running executable: {e}"))
}

fn manager_processes(request: &Request) -> Result<Vec<Process>, String> {
    let snapshot = Process(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
            .map_err(|e| format!("Cannot enumerate running tile-rs processes: {e}"))?,
    );
    let expected = request.exe.canonicalize().map_err(|e| e.to_string())?;
    let filename = request
        .exe
        .file_name()
        .ok_or("Invalid update executable")?
        .to_string_lossy();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut result = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    let mut processes = Vec::new();
    loop {
        if let Err(error) = result {
            if error.code() == ERROR_NO_MORE_FILES.to_hresult() {
                return Ok(processes);
            }
            return Err(format!(
                "Cannot enumerate running tile-rs processes: {error}"
            ));
        }
        let len = entry
            .szExeFile
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = OsString::from_wide(&entry.szExeFile[..len]);
        if entry.th32ProcessID != request.parent
            && entry.th32ProcessID != std::process::id()
            && name.to_string_lossy().eq_ignore_ascii_case(&filename)
        {
            match open_process(entry.th32ProcessID) {
                Ok(process) => {
                    // Retain this exact process object, never look up the PID again after waiting.
                    if unsafe { WaitForSingleObject(process.0, 0) } != WAIT_OBJECT_0
                        && process_image(&process)? == expected
                    {
                        processes.push(process);
                    }
                }
                Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => {}
                Err(error) => {
                    return Err(format!(
                        "Cannot wait for a running tile-rs process: {error}"
                    ))
                }
            }
        }
        result = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
}

fn wait_for_exit(process: &Process, milliseconds: u32, name: &str) -> Result<(), String> {
    match unsafe { WaitForSingleObject(process.0, milliseconds) } {
        WAIT_OBJECT_0 => Ok(()),
        WAIT_TIMEOUT => Err(format!("{name} is still running. Close it and try again.")),
        _ => Err(format!("Could not wait for {name} to exit.")),
    }
}

fn installer_parameters(target: &Path, autostart: bool) -> OsString {
    let mut parameters =
        OsString::from("/SILENT /NORESTART /SUPPRESSMSGBOXES /NOCLOSEAPPLICATIONS /DIR=\"");
    parameters.push(target);
    parameters.push(if autostart {
        "\" /MERGETASKS=autostart"
    } else {
        "\" /MERGETASKS=!autostart"
    });
    parameters
}

fn check_installer_exit(code: u32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "Installer exited with code {code}. Your previous settings have been kept."
        ))
    }
}

fn run_installer(request: &Request) -> Result<(), String> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
        .ok()
        .map_err(|e| format!("Cannot initialize the update installer: {e}"))?;
    let result = (|| {
        let file: Vec<_> = request
            .setup
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let parameters: Vec<_> = installer_parameters(&request.target, request.autostart)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_FLAG_NO_UI,
            lpVerb: w!("open"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(parameters.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        unsafe { ShellExecuteExW(&mut info) }
            .map_err(|e| format!("Cannot start the update installer: {e}"))?;
        let process = Process(info.hProcess);
        if process.0.is_invalid() {
            return Err("The installer returned no process handle.".into());
        }
        wait_for_exit(&process, INFINITE, "The installer")?;
        let mut code = 0;
        unsafe { GetExitCodeProcess(process.0, &mut code) }.map_err(|e| e.to_string())?;
        check_installer_exit(code)
    })();
    unsafe {
        CoUninitialize();
    }
    result
}

fn apply(request: &Request, parent: &Process, managers: &[Process]) -> Result<(), String> {
    let mut ready = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request.ready)
        .map_err(|e| format!("Cannot confirm update helper readiness: {e}"))?;
    ready
        .write_all(b"ready")
        .and_then(|_| ready.sync_all())
        .map_err(|e| e.to_string())?;
    drop(ready);
    wait_for_exit(parent, 60000, "The launcher")?;
    for manager in managers {
        wait_for_exit(manager, 30000, "tile-rs")?;
    }
    run_installer(request)
}

pub(crate) fn run() {
    let prepared = (|| {
        let request = Request::parse(|name| std::env::var_os(name))?;
        request.validate()?;
        let parent = open_process(request.parent)
            .map_err(|e| format!("Cannot wait for the launcher: {e}"))?;
        if process_image(&parent)? != request.exe.canonicalize().map_err(|e| e.to_string())? {
            return Err("The update parent is not the expected launcher.".into());
        }
        let managers = manager_processes(&request)?;
        Ok::<_, String>((request, parent, managers))
    })();
    let (request, parent, managers) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            crate::platform::dialogs::msgbox("tile-rs update failed", &error, false);
            return;
        }
    };
    let result = apply(&request, &parent, &managers);
    if let Err(error) = result {
        crate::platform::dialogs::msgbox("tile-rs update failed", &error, false);
    }
    if unsafe { WaitForSingleObject(parent.0, 0) } == WAIT_OBJECT_0 && request.exe.is_file() {
        if let Err(error) = Command::new(&request.exe)
            .creation_flags(0x08000000)
            .spawn()
        {
            crate::platform::dialogs::msgbox(
                "tile-rs update failed",
                &format!("Cannot restart tile-rs: {error}"),
                false,
            );
        }
    }
    let _ = std::fs::remove_file(&request.ready);
    let _ = std::fs::remove_file(&request.setup);
    // Windows holds the running helper executable open; its copy remains in this TEMP folder.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(name: &str) -> Option<OsString> {
        Some(OsString::from(match name {
            "TILE_UPDATE_PARENT" => "4294967294",
            "TILE_UPDATE_EXE" => r"C:\Program Files\tile-rs\tile-rs.exe",
            "TILE_UPDATE_TARGET" => r"C:\Program Files\tile-rs",
            "TILE_UPDATE_SETUP" => r"C:\Temp\update\setup.exe",
            "TILE_UPDATE_READY" => r"C:\Temp\update\ready",
            "TILE_UPDATE_AUTOSTART" => "1",
            _ => return None,
        }))
    }

    #[test]
    fn helper_inputs_reject_missing_values_relative_paths_and_invalid_choices() {
        assert!(Request::parse(inputs).is_ok());
        assert!(Request::parse(|_| None).is_err());
        for (name, value) in [
            ("TILE_UPDATE_PARENT", "0"),
            ("TILE_UPDATE_PARENT", "invalid"),
            ("TILE_UPDATE_EXE", "relative.exe"),
            ("TILE_UPDATE_AUTOSTART", "yes"),
        ] {
            assert!(Request::parse(|key| if key == name {
                Some(value.into())
            } else {
                inputs(key)
            })
            .is_err());
        }
    }

    #[test]
    fn installer_parameters_quote_directory_and_preserve_autostart_choice() {
        let directory = Path::new(r"C:\Program Files\Пример");
        assert_eq!(
            installer_parameters(directory, true),
            OsString::from(
                r#"/SILENT /NORESTART /SUPPRESSMSGBOXES /NOCLOSEAPPLICATIONS /DIR="C:\Program Files\Пример" /MERGETASKS=autostart"#
            )
        );
        assert!(installer_parameters(directory, false)
            .to_string_lossy()
            .ends_with("/MERGETASKS=!autostart"));
        assert!(check_installer_exit(0).is_ok());
        assert!(check_installer_exit(5).unwrap_err().contains("code 5"));
    }

    #[test]
    fn helper_path_inputs_preserve_utf16_without_lossy_conversion() {
        let path = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800]);
        let request = Request::parse(|name| {
            if name == "TILE_UPDATE_TARGET" {
                Some(path.clone())
            } else {
                inputs(name)
            }
        })
        .unwrap();
        assert_eq!(request.target.as_os_str(), path);
    }
}
