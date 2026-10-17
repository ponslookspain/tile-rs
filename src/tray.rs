//! System-tray icon and its context menu.

use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, GetCursorPos, PostMessageW, PostQuitMessage, RegisterClassW,
    RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu, HICON, ICONINFO, MF_CHECKED,
    MF_SEPARATOR, MF_STRING, SW_SHOWNORMAL, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE,
    WM_APP, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_OVERLAPPED,
};

use crate::config::{self, Action};

const WM_TRAY: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;

static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

// menu command ids
const M_PAUSE: usize = 1;
const M_RETILE: usize = 2;
const M_RELOAD: usize = 3;
const M_EDIT: usize = 4;
const M_FOLDER: usize = 5;
const M_LOG: usize = 6;
const M_AUTOSTART: usize = 7;
const M_HOTKEYS: usize = 8;
const M_QUIT: usize = 9;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Draws a small 3-tile icon (blue when active, grey when paused).
fn make_icon(active: bool) -> Option<HICON> {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4]; // BGRA
    let (r, g, b) = if active { (0x8d_u8, 0xbc_u8, 0xff_u8) } else { (0x90, 0x90, 0x90) };
    let mut fill = |x0: usize, y0: usize, x1: usize, y1: usize, shade: f32| {
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * N + x) * 4;
                px[i] = (b as f32 * shade) as u8;
                px[i + 1] = (g as f32 * shade) as u8;
                px[i + 2] = (r as f32 * shade) as u8;
                px[i + 3] = 255;
            }
        }
    };
    fill(2, 2, 15, 30, 1.0); // big tile on the left
    fill(17, 2, 30, 15, 0.8); // top right
    fill(17, 17, 30, 30, 0.6); // bottom right
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
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
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

fn item(menu: windows::Win32::UI::WindowsAndMessaging::HMENU, id: usize, text: &str, checked: bool) {
    let t = wide(text);
    let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
    unsafe {
        let _ = AppendMenuW(menu, flags, id, PCWSTR(t.as_ptr()));
    }
}

fn show_menu(hwnd: HWND) {
    let (_, paused) = crate::ui_state();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        item(menu, M_PAUSE, if paused { "Resume tiling" } else { "Pause tiling" }, false);
        item(menu, M_RETILE, "Re-tile now", false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(menu, M_EDIT, "Edit config (hotkeys, rules)...", false);
        item(menu, M_RELOAD, "Reload config", false);
        item(menu, M_HOTKEYS, "Show hotkeys", false);
        item(menu, M_FOLDER, "Open config folder", false);
        item(menu, M_LOG, "Open log", false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(menu, M_AUTOSTART, "Start with Windows", crate::autostart_enabled());
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        item(menu, M_QUIT, "Quit tile-rs", false);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // required so the menu closes when the user clicks elsewhere
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);

        match cmd.0 as usize {
            M_PAUSE => crate::dispatch(Action::Pause),
            M_RETILE => crate::dispatch(Action::Retile),
            M_RELOAD => crate::dispatch(Action::ReloadConfig),
            M_EDIT => crate::dispatch(Action::EditConfig),
            M_HOTKEYS => show_hotkeys(),
            M_FOLDER => open("explorer.exe", &format!("\"{}\"", config::config_dir().display())),
            M_LOG => open("notepad.exe", &format!("\"{}\"", crate::log_file().display())),
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
        ShellExecuteW(None, w!("open"), PCWSTR(e.as_ptr()), PCWSTR(a.as_ptr()), PCWSTR::null(), SW_SHOWNORMAL);
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
    if msg == WM_TRAY {
        match lparam.0 as u32 {
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
    DefWindowProcW(hwnd, msg, wparam, lparam)
}
