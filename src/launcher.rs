//! The settings launcher: a small native window with the main options, the layout picker and a
//! "check for updates" link. It runs in the same process as the window manager, so saving writes
//! `config.toml` and applies it right away.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRect, CreateWindowExW, DefWindowProcW, DestroyWindow, GetSystemMetrics,
    GetWindowTextW, IsIconic, RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowTextW,
    ShowWindow, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_GROUPBOX,
    BS_PUSHBUTTON, CBS_DROPDOWNLIST, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, ES_AUTOHSCROLL,
    ES_AUTOVSCROLL, ES_MULTILINE, HMENU, SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_RESTORE, SW_SHOW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_SETFONT, WNDCLASSW,
    WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_DISABLED, WS_MINIMIZEBOX, WS_SYSMENU,
    WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{self, FullscreenWindows, Layout};
use crate::geometry::Easing;
use crate::tray;

const GITHUB: &str = "https://github.com/ponslookspain/tile-rs";

// control ids
const ID_SAVE: usize = 100;
const ID_UPDATES: usize = 101;
const ID_CONFIG: usize = 102;
const ID_LOG: usize = 103;
const ID_QUIT: usize = 104;

const CLIENT_W: i32 = 540;
const CLIENT_H: i32 = 785;
const WIN_STYLE: u32 = WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0;

// styles (windows-rs 0.61: WS_* are WINDOW_STYLE, BS_/ES_/CBS_ are plain i32)
const S_STATIC: u32 = WS_CHILD.0 | WS_VISIBLE.0;
const S_EDIT: u32 = WS_CHILD.0 | WS_VISIBLE.0 | WS_BORDER.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32;
const S_CHECK: u32 = WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32;
const S_RADIO_ON: u32 = WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_AUTORADIOBUTTON as u32;
const S_RADIO_OFF: u32 = WS_CHILD.0 | WS_VISIBLE.0 | WS_DISABLED.0 | BS_AUTORADIOBUTTON as u32;
const S_GROUP: u32 = WS_CHILD.0 | WS_VISIBLE.0 | BS_GROUPBOX as u32;
const S_COMBO: u32 =
    WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32;
const S_BUTTON: u32 = WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32;
const S_HOTKEYS: u32 = WS_CHILD.0
    | WS_VISIBLE.0
    | WS_BORDER.0
    | WS_VSCROLL.0
    | WS_TABSTOP.0
    | ES_MULTILINE as u32
    | ES_AUTOVSCROLL as u32;

static LAUNCHER: AtomicIsize = AtomicIsize::new(0);

#[derive(Default)]
struct Controls {
    gap: HWND,
    animations: HWND,
    anim_ms: HWND,
    easing: HWND,
    resize_step: HWND,
    layout: [HWND; 4],
    min_w: HWND,
    min_h: HWND,
    max_tiles: HWND,
    auto_overflow: HWND,
    indicator: HWND,
    border: HWND,
    border_color: HWND,
    border_width: HWND,
    fullscreen: HWND,
    hotkeys: HWND,
    autostart: HWND,
}

thread_local! {
    static C: RefCell<Controls> = RefCell::new(Controls::default());
}

fn wide(s: &str) -> Vec<u16> {
    tray::wide(s)
}

fn sm(h: HWND, msg: u32, wparam: usize, lparam: isize) -> isize {
    unsafe { SendMessageW(h, msg, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
}

#[allow(clippy::too_many_arguments)]
unsafe fn mk(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: u32,
    id: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> HWND {
    let hmod = GetModuleHandleW(None).unwrap_or_default();
    let t = wide(text);
    let h = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR(t.as_ptr()),
        WINDOW_STYLE(style),
        x,
        y,
        w,
        h,
        Some(parent),
        Some(HMENU(id as *mut _)),
        Some(HINSTANCE(hmod.0)),
        None,
    )
    .unwrap_or_default();
    let font = GetStockObject(DEFAULT_GUI_FONT);
    sm(h, WM_SETFONT, font.0 as usize, 1);
    h
}

fn label(parent: HWND, text: &str, x: i32, y: i32) {
    unsafe {
        mk(parent, w!("STATIC"), text, S_STATIC, 0, x, y, 280, 20);
    }
}

fn edit(parent: HWND, x: i32, y: i32, w: i32) -> HWND {
    unsafe { mk(parent, w!("EDIT"), "", S_EDIT, 0, x, y, w, 22) }
}

fn check(parent: HWND, text: &str, x: i32, y: i32, w: i32, on: bool) -> HWND {
    unsafe {
        let h = mk(parent, w!("BUTTON"), text, S_CHECK, 0, x, y, w, 20);
        sm(h, BM_SETCHECK, usize::from(on), 0);
        h
    }
}

fn radio(parent: HWND, text: &str, x: i32, y: i32, enabled: bool, on: bool) -> HWND {
    unsafe {
        let h = mk(
            parent,
            w!("BUTTON"),
            text,
            if enabled { S_RADIO_ON } else { S_RADIO_OFF },
            0,
            x,
            y,
            320,
            20,
        );
        sm(h, BM_SETCHECK, usize::from(on), 0);
        h
    }
}

fn group(parent: HWND, text: &str, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
        mk(parent, w!("BUTTON"), text, S_GROUP, 0, x, y, w, h);
    }
}

fn combo(parent: HWND, items: &[&str], sel: usize, x: i32, y: i32, w: i32) -> HWND {
    unsafe {
        let h = mk(parent, w!("COMBOBOX"), "", S_COMBO, 0, x, y, w, 200);
        for it in items {
            let t = wide(it);
            sm(h, CB_ADDSTRING, 0, t.as_ptr() as isize);
        }
        sm(h, CB_SETCURSEL, sel, 0);
        h
    }
}

fn button(parent: HWND, text: &str, id: usize, x: i32, y: i32, w: i32) -> HWND {
    unsafe { mk(parent, w!("BUTTON"), text, S_BUTTON, id, x, y, w, 30) }
}

fn get_text(h: HWND) -> String {
    let mut buf = [0u16; 8192];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn set_text(h: HWND, s: &str) {
    let t = wide(s);
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
    }
}

fn is_checked(h: HWND) -> bool {
    sm(h, BM_GETCHECK, 0, 0) == 1 // BST_CHECKED
}

fn set_check(h: HWND, on: bool) {
    sm(h, BM_SETCHECK, usize::from(on), 0);
}

fn combo_sel(h: HWND) -> usize {
    let r = sm(h, CB_GETCURSEL, 0, 0);
    if r < 0 {
        0
    } else {
        r as usize
    }
}

const EASINGS: [Easing; 5] = [
    Easing::Linear,
    Easing::OutQuad,
    Easing::OutCubic,
    Easing::OutQuart,
    Easing::InOutCubic,
];
const EASING_NAMES: [&str; 5] = [
    "linear",
    "out_quad",
    "out_cubic",
    "out_quart",
    "in_out_cubic",
];

fn build_controls(parent: HWND) {
    let mut c = Controls::default();
    unsafe {
        group(parent, "General", 10, 10, 520, 170);
        label(parent, "Gap between windows (px):", 20, 40);
        c.gap = edit(parent, 200, 37, 60);
        c.animations = check(parent, "Smooth animations", 20, 68, 300, true);
        label(parent, "Animation duration (ms):", 20, 96);
        c.anim_ms = edit(parent, 200, 93, 60);
        label(parent, "Easing:", 20, 124);
        c.easing = combo(parent, &EASING_NAMES, 2, 200, 122, 160);
        label(parent, "Resize step (px):", 360, 40);
        c.resize_step = edit(parent, 470, 37, 50);

        group(parent, "Layout", 10, 190, 520, 215);
        c.layout[0] = radio(parent, "Spiral (dwindle)", 20, 220, true, true);
        c.layout[1] = radio(
            parent,
            "Master - stack   (coming soon)",
            20,
            245,
            false,
            false,
        );
        c.layout[2] = radio(parent, "Grid   (coming soon)", 20, 270, false, false);
        c.layout[3] = radio(parent, "Columns   (coming soon)", 20, 295, false, false);
        label(parent, "Min tile width (px):", 300, 220);
        c.min_w = edit(parent, 460, 217, 60);
        label(parent, "Min tile height (px):", 300, 245);
        c.min_h = edit(parent, 460, 242, 60);
        label(parent, "Max tiles (0 = auto):", 300, 270);
        c.max_tiles = edit(parent, 460, 267, 60);
        c.auto_overflow = check(
            parent,
            "When this workspace is full, open a new window on the next one",
            20,
            330,
            500,
            true,
        );
        c.indicator = check(
            parent,
            "Show the workspace indicator (dots at the top)",
            20,
            355,
            500,
            true,
        );

        group(parent, "Border", 10, 415, 520, 120);
        c.border = check(
            parent,
            "Draw a border around the focused window",
            20,
            445,
            500,
            true,
        );
        label(parent, "Colour (#rrggbb):", 20, 473);
        c.border_color = edit(parent, 200, 470, 100);
        label(parent, "Width (px):", 330, 445);
        c.border_width = edit(parent, 460, 442, 60);
        label(parent, "Fullscreen windows:", 20, 501);
        c.fullscreen = combo(parent, &["minimize", "ignore"], 0, 200, 499, 160);

        group(
            parent,
            "Hotkeys  (one per line, e.g.  focus_left = alt+h)",
            10,
            545,
            520,
            150,
        );
        c.hotkeys = mk(parent, w!("EDIT"), "", S_HOTKEYS, 0, 20, 570, 500, 115);

        button(parent, "Save && apply", ID_SAVE, 10, 705, 120);
        button(parent, "Check for updates", ID_UPDATES, 140, 705, 150);
        button(parent, "Open config", ID_CONFIG, 300, 705, 110);
        button(parent, "Open log", ID_LOG, 420, 705, 100);
        c.autostart = check(parent, "Start with Windows", 10, 748, 300, false);
        button(parent, "Quit tile-rs", ID_QUIT, 430, 742, 100);
    }
    C.with(|cc| *cc.borrow_mut() = c);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            build_controls(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xFFFF {
                ID_SAVE => save_and_apply(hwnd),
                ID_UPDATES => tray::open(GITHUB, ""),
                ID_CONFIG => {
                    config::ensure_default();
                    tray::open(
                        "notepad.exe",
                        &format!("\"{}\"", config::config_path().display()),
                    );
                }
                ID_LOG => tray::open(
                    "notepad.exe",
                    &format!("\"{}\"", crate::log_file().display()),
                ),
                ID_QUIT => crate::dispatch_quit(),
                _ => {}
            }
            LRESULT(0)
        }
        // closing the launcher just hides it; tile-rs keeps running in the tray
        WM_CLOSE => {
            hide();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Reads the controls, writes `config.toml` (in place) and applies it.
fn save_and_apply(hwnd: HWND) {
    let (pairs, hotkeys, autostart) = C.with(|cc| {
        let c = cc.borrow();
        let layout = if is_checked(c.layout[1]) {
            Layout::MasterStack
        } else if is_checked(c.layout[2]) {
            Layout::Grid
        } else if is_checked(c.layout[3]) {
            Layout::Columns
        } else {
            Layout::Spiral
        };
        let easing = EASING_NAMES[combo_sel(c.easing).min(EASINGS.len() - 1)];
        let fullscreen = if combo_sel(c.fullscreen) == 1 {
            "ignore"
        } else {
            "minimize"
        };
        let num = |h: HWND| get_text(h).trim().to_string();
        let pairs: Vec<(&str, String)> = vec![
            ("layout", format!("\"{}\"", layout.name())),
            ("gap", num(c.gap)),
            ("animations", is_checked(c.animations).to_string()),
            ("animation_ms", num(c.anim_ms)),
            ("easing", format!("\"{easing}\"")),
            ("resize_step", num(c.resize_step)),
            ("min_tile_width", num(c.min_w)),
            ("min_tile_height", num(c.min_h)),
            ("max_tiles", num(c.max_tiles)),
            ("auto_overflow", is_checked(c.auto_overflow).to_string()),
            ("workspace_indicator", is_checked(c.indicator).to_string()),
            ("border", is_checked(c.border).to_string()),
            ("border_color", format!("\"{}\"", num(c.border_color))),
            ("border_width", num(c.border_width)),
            ("fullscreen_windows", format!("\"{fullscreen}\"")),
        ];
        (pairs, get_text(c.hotkeys), is_checked(c.autostart))
    });
    let path = config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let new = config::edit_top(&text, &pairs);
    let _ = std::fs::write(&path, new);
    let _ = write_hotkeys(&hotkeys);
    crate::set_autostart(autostart);
    crate::reload_config();
    crate::notify("tile-rs", "Settings saved.");
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
}

/// Replaces the `[hotkeys]` section with `lines` (raw `action = key` lines), leaving the rest.
/// Each value is normalised so the result is always valid TOML: a bare key is quoted, an already
/// quoted string or a `[...]` list is kept as it is.
fn write_hotkeys(lines: &str) -> std::io::Result<()> {
    let path = config::config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut hot: Vec<String> = Vec::new();
    for l in lines.lines() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            let value = if v.starts_with('[') || v.starts_with('"') {
                v.to_string()
            } else {
                format!("\"{}\"", v.trim_matches('"'))
            };
            hot.push(format!("{k} = {value}"));
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut in_hotkeys = false;
    let mut found = false;
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_hotkeys = t == "[hotkeys]";
            found |= in_hotkeys;
            out.push(l.to_string());
            if in_hotkeys {
                out.extend(hot.iter().cloned());
            }
            continue;
        }
        if in_hotkeys {
            continue; // drop the old hotkeys
        }
        out.push(l.to_string());
    }
    if !found {
        out.push(String::new());
        out.push("[hotkeys]".into());
        out.extend(hot);
    }
    let mut s = out.join(nl);
    s.push_str(nl);
    std::fs::write(path, s)
}

fn populate() {
    let c = config::current();
    C.with(|cc| {
        let cc = cc.borrow();
        set_text(cc.gap, &c.gap.to_string());
        set_check(cc.animations, c.animations);
        set_text(cc.anim_ms, &c.animation_ms.to_string());
        let ei = EASINGS.iter().position(|e| *e == c.easing).unwrap_or(2);
        sm(cc.easing, CB_SETCURSEL, ei, 0);
        set_text(cc.resize_step, &c.resize_step.to_string());
        let li = match c.layout {
            Layout::Spiral => 0,
            Layout::MasterStack => 1,
            Layout::Grid => 2,
            Layout::Columns => 3,
        };
        for (i, h) in cc.layout.iter().enumerate() {
            set_check(*h, i == li);
        }
        set_text(cc.min_w, &c.min_tile_width.to_string());
        set_text(cc.min_h, &c.min_tile_height.to_string());
        set_text(cc.max_tiles, &c.max_tiles.to_string());
        set_check(cc.auto_overflow, c.auto_overflow);
        set_check(cc.indicator, c.workspace_indicator);
        set_check(cc.border, c.border);
        set_check(cc.autostart, crate::autostart_enabled());
        sm(
            cc.fullscreen,
            CB_SETCURSEL,
            usize::from(c.fullscreen_windows == FullscreenWindows::Ignore),
            0,
        );
        let col = c.border_color;
        set_text(
            cc.border_color,
            &format!(
                "#{:02x}{:02x}{:02x}",
                col & 0xFF,
                (col >> 8) & 0xFF,
                (col >> 16) & 0xFF
            ),
        );
        set_text(cc.border_width, &c.border_width.to_string());
        set_text(cc.hotkeys, &hotkeys_text());
    });
}

/// The `[hotkeys]` body for the editor: one line per action, with the binding(s) quoted
/// (`focus_left = ["alt+h", "alt+left"]`), so the text is valid TOML as-is.
fn hotkeys_text() -> String {
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for (action, spec) in &config::current().bindings {
        let name = crate::action_name(*action);
        if !map.contains_key(&name) {
            order.push(name.clone());
        }
        map.entry(name).or_default().push(spec.clone());
    }
    let mut out = String::new();
    for name in order {
        let specs = &map[&name];
        if specs.len() == 1 {
            out.push_str(&format!("{name} = \"{}\"\n", specs[0]));
        } else {
            let list = specs
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("{name} = [{list}]\n"));
        }
    }
    out
}

/// Creates the (hidden) launcher window.
pub fn create() -> bool {
    unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        let hinst = HINSTANCE(hmod.0);
        let class = w!("tile-rs-launcher");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: CLIENT_W,
            bottom: CLIENT_H,
        };
        let _ = AdjustWindowRect(&mut rc, WINDOW_STYLE(WIN_STYLE), false);
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - h) / 2;
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("tile-rs settings"),
            WINDOW_STYLE(WIN_STYLE),
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return false;
        };
        LAUNCHER.store(hwnd.0 as isize, Ordering::Relaxed);
        true
    }
}

pub fn show() {
    let raw = LAUNCHER.load(Ordering::Relaxed);
    if raw == 0 {
        return;
    }
    populate();
    unsafe {
        let hwnd = HWND(raw as *mut _);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn hide() {
    let raw = LAUNCHER.load(Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = ShowWindow(HWND(raw as *mut _), SW_HIDE);
        }
    }
}

pub fn destroy() {
    let raw = LAUNCHER.swap(0, Ordering::Relaxed);
    if raw != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(raw as *mut _));
        }
    }
}
