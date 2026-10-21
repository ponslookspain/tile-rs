//! System-tray icon and its context menu.

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIN_BALLOONUSERCLICK, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, GetCursorPos, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, HICON, ICONINFO, MF_CHECKED,
    MF_GRAYED, MF_SEPARATOR, MF_STRING, SPI_SETWORKAREA, SW_SHOWNORMAL, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE, WM_LBUTTONDBLCLK,
    WM_NULL, WM_RBUTTONUP, WM_SETTINGCHANGE, WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPED,
};

use crate::config::{self, Action};

const WM_TRAY: u32 = WM_APP + 1;
const WM_LAUNCHER_RELOAD: u32 = WM_APP + 2;
const WM_LAUNCHER_QUIT: u32 = WM_APP + 3;
const WM_DIAGNOSTICS: u32 = WM_APP + 4;
const WM_DIAGNOSTICS_WITH_TITLES: u32 = WM_APP + 5;
const TRAY_UID: u32 = 1;

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

// menu command ids
const M_SETTINGS: usize = 1;
const M_PAUSE: usize = 2;
const M_RETILE: usize = 3;
const M_RELOAD: usize = 4;
const M_EDIT: usize = 5;
const M_FOLDER: usize = 6;
const M_LOG: usize = 7;
const M_DIAGNOSTICS: usize = 12;
const M_AUTOSTART: usize = 8;
const M_HOTKEYS: usize = 9;
const M_UPDATE: usize = 11;
const M_QUIT: usize = 10;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Draws a small 3-tile icon (blue when active, grey when paused).
fn make_icon(active: bool) -> Option<HICON> {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4]; // BGRA
    let (r, g, b) = if active {
        (0x8d_u8, 0xbc_u8, 0xff_u8)
    } else {
        (0x90, 0x90, 0x90)
    };
    // rounded tiles with a soft (anti-aliased) edge
    let mut fill = |x0: f32, y0: f32, x1: f32, y1: f32, rad: f32, shade: f32| {
        for y in 0..N {
            for x in 0..N {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                if fx < x0 || fx > x1 || fy < y0 || fy > y1 {
                    continue;
                }
                let dx = (x0 + rad - fx).max(fx - (x1 - rad)).max(0.0);
                let dy = (y0 + rad - fy).max(fy - (y1 - rad)).max(0.0);
                let cov = (rad - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let i = (y * N + x) * 4;
                px[i] = (b as f32 * shade) as u8;
                px[i + 1] = (g as f32 * shade) as u8;
                px[i + 2] = (r as f32 * shade) as u8;
                px[i + 3] = (255.0 * cov) as u8;
            }
        }
    };
    fill(2.0, 2.0, 15.0, 30.0, 3.5, 1.0); // big tile on the left
    fill(17.0, 2.0, 30.0, 15.0, 3.0, 0.85); // top right
    fill(17.0, 17.0, 30.0, 30.0, 3.0, 0.65); // bottom right
    unsafe {
        let color = CreateBitmap(N as i32, N as i32, 1, 32, Some(px.as_ptr() as *const _));
        let mask = CreateBitmap(N as i32, N as i32, 1, 1, None);
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon
    }
}

fn nid(hwnd: HWND, icon: HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = TRAY_UID;
    d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = icon;
    for (i, c) in tip.encode_utf16().take(127).enumerate() {
        d.szTip[i] = c;
    }
    d
}

fn tip_and_state() -> (String, bool) {
    let (cur, paused) = crate::ui_state();
    let tip = if paused {
        format!("tile-rs - paused (workspace {})", cur + 1)
    } else {
        format!("tile-rs - workspace {}", cur + 1)
    };
    (tip, !paused)
}

fn add(hwnd: HWND) {
    let (tip, active) = tip_and_state();
    if let Some(icon) = make_icon(active) {
        let d = nid(hwnd, icon, &tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &d);
            let _ = DestroyIcon(icon);
        }
    }
}

/// Refresh tooltip and icon colour (call after pause/resume, workspace switch, reload).
pub fn update() {
    let raw = TRAY_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let hwnd = HWND(raw as *mut _);
    let (tip, active) = tip_and_state();
    if let Some(icon) = make_icon(active) {
        let d = nid(hwnd, icon, &tip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
            let _ = DestroyIcon(icon);
        }
    }
}

/// Show a non-modal release notification using the manager's existing tray icon.
pub fn notify_update(version: &str) {
    let raw = TRAY_HWND.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(raw as *mut _),
        uID: TRAY_UID,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    for (dest, ch) in data
        .szInfoTitle
        .iter_mut()
        .take(63)
        .zip("tile-rs: доступно обновление".encode_utf16())
    {
        *dest = ch;
    }
    let message = format!(
        "Версия {version} готова. Нажмите, чтобы открыть Check update и установить обновление."
    );
    for (dest, ch) in data.szInfo.iter_mut().take(255).zip(message.encode_utf16()) {
        *dest = ch;
    }
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

/// Ask the running manager process to reload the config after the launcher saved it.
pub fn request_manager_reload() -> bool {
    post_manager_message(WM_LAUNCHER_RELOAD)
}

/// Ask the running manager process to quit from the launcher.
pub fn request_manager_quit() -> bool {
    post_manager_message(WM_LAUNCHER_QUIT)
}

pub fn request_manager_diagnostics(include_titles: bool) -> bool {
    post_manager_message(if include_titles {
        WM_DIAGNOSTICS_WITH_TITLES
    } else {
        WM_DIAGNOSTICS
    })
}

fn post_manager_message(message: u32) -> bool {
    unsafe {
        let Ok(hwnd) = windows::Win32::UI::WindowsAndMessaging::FindWindowW(
            w!("tile-rs-tray"),
            PCWSTR::null(),
        ) else {
            return false;
        };
        !hwnd.0.is_null() && PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0)).is_ok()
    }
}

pub fn remove() {
    let raw = TRAY_HWND.swap(0, Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    let hwnd = HWND(raw as *mut _);
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = TRAY_UID;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &d);
    }
}

/// Creates the hidden window that receives tray messages and adds the icon.
pub fn create() {
    unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        let hinst = HINSTANCE(hmod.0);
        let class = w!("tile-rs-tray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            Ordering::Relaxed,
        );
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0) | WS_EX_TOOLWINDOW,
            class,
            w!("tile-rs"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            crate::log("could not create the tray window");
            return;
        };
        TRAY_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        add(hwnd);
    }
}

fn item(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    id: usize,
    text: &str,
    checked: bool,
) {
    let t = wide(text);
    let flags = if checked {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    };
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR(t.as_ptr()));
    }
}

fn show_menu(hwnd: HWND) {
    let (_, paused) = crate::ui_state();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let ver = wide(&format!("tile-rs v{}", env!("CARGO_PKG_VERSION")));
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(ver.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(menu, M_SETTINGS, "Settings...", false);
        item(
            menu,
            M_PAUSE,
            if paused {
                "Resume tiling"
            } else {
                "Pause tiling"
            },
            false,
        );
        item(menu, M_RETILE, "Re-tile now", false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(menu, M_EDIT, "Edit config (hotkeys, rules)...", false);
        item(menu, M_RELOAD, "Reload config", false);
        item(menu, M_HOTKEYS, "Show hotkeys", false);
        item(menu, M_FOLDER, "Open config folder", false);
        item(menu, M_LOG, "Open log", false);
        item(menu, M_DIAGNOSTICS, "Export window diagnostics", false);
        item(menu, M_UPDATE, "Check for updates", false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(
            menu,
            M_AUTOSTART,
            "Start with Windows",
            crate::autostart_enabled(),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(menu, M_QUIT, "Quit tile-rs", false);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // required so the menu closes when the user clicks elsewhere
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        match cmd.0 as usize {
            M_SETTINGS => crate::show_launcher(),
            M_PAUSE => crate::dispatch(Action::Pause),
            M_RETILE => crate::dispatch(Action::Retile),
            M_RELOAD => crate::dispatch(Action::ReloadConfig),
            M_DIAGNOSTICS => crate::export_diagnostics(false),
            M_EDIT => crate::dispatch(Action::EditConfig),
            M_HOTKEYS => show_hotkeys(),
            M_FOLDER => open(
                "explorer.exe",
                &format!("\"{}\"", config::config_dir().display()),
            ),
            M_LOG => open(
                "notepad.exe",
                &format!("\"{}\"", crate::log_file().display()),
            ),
            M_UPDATE => crate::update::check_and_notify(),
            M_AUTOSTART => {
                let want = !crate::autostart_enabled();
                if !crate::set_autostart(want) {
                    crate::notify("tile-rs", "Could not change the autostart setting.");
                }
            }
            M_QUIT => crate::dispatch(Action::Quit),
            _ => {}
        }
    }
}

/// Opens `exe` with `args` (used for notepad / explorer).
pub fn open(exe: &str, args: &str) {
    let e = wide(exe);
    let a = wide(args);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(e.as_ptr()),
            PCWSTR(a.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn show_hotkeys() {
    let cfg = config::current();
    let mut text = String::new();
    let mut last_name = String::new();
    for (action, spec) in &cfg.bindings {
        let name = crate::action_name(*action);
        if name == last_name {
            text.push_str(&format!(", {spec}"));
        } else {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!("{name}: {spec}"));
            last_name = name;
        }
    }
    if text.is_empty() {
        text = "No hotkeys are configured.".into();
    }
    text.push_str("\n\nEdit them via the tray menu: Edit config.");
    crate::notify("tile-rs hotkeys", &text);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_DIAGNOSTICS || msg == WM_DIAGNOSTICS_WITH_TITLES {
        crate::export_diagnostics(msg == WM_DIAGNOSTICS_WITH_TITLES);
        return LRESULT(0);
    }
    if msg == WM_LAUNCHER_RELOAD {
        crate::reload_config();
        return LRESULT(0);
    }
    if msg == WM_LAUNCHER_QUIT {
        crate::dispatch_quit();
        return LRESULT(0);
    }
    if msg == WM_TRAY {
        match lparam.0 as u32 {
            NIN_BALLOONUSERCLICK => crate::launcher::show_updates(),
            WM_RBUTTONUP => show_menu(hwnd),
            WM_LBUTTONDBLCLK => crate::dispatch(Action::Pause),
            _ => {}
        }
        return LRESULT(0);
    }
    if msg == TASKBAR_CREATED.load(Ordering::Relaxed) && msg != 0 {
        add(hwnd); // Explorer restarted: put the icon back
        return LRESULT(0);
    }
    if msg == WM_DESTROY {
        PostQuitMessage(0);
        return LRESULT(0);
    }
    // the work area changed (resolution, taskbar size / auto-hide): rebuild the layout
    if msg == WM_DISPLAYCHANGE
        || (msg == WM_SETTINGCHANGE && wparam.0 == SPI_SETWORKAREA.0 as usize)
    {
        crate::request_relayout();
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
