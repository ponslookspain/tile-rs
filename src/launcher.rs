//! The settings launcher: a small native window with the main options, the layout picker and a
//! "check for updates" link. It runs in the same process as the window manager, so saving writes
//! `config.toml` and applies it right away.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, GetStockObject, SetBkColor, SetTextColor,
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_GUI_FONT, DEFAULT_PITCH,
    FW_NORMAL, HFONT, HGDIOBJ, OUT_DEFAULT_PRECIS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetSystemMetrics, GetWindowTextW, IsIconic,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetWindowTextW, ShowWindow, BM_GETCHECK,
    BM_SETCHECK, BS_AUTOCHECKBOX, BS_AUTORADIOBUTTON, BS_GROUPBOX, BS_PUSHBUTTON, CBS_DROPDOWNLIST,
    CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, HMENU,
    SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_RESTORE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX,
    WM_CTLCOLORSTATIC, WM_SETFONT, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN,
    WS_DISABLED, WS_MINIMIZEBOX, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use crate::config::{self, FullscreenWindows, IndicatorSize, Layout, MediaWindows};
use crate::geometry::Easing;
use crate::tray;

// control ids
const ID_SAVE: usize = 100;
const ID_UPDATES: usize = 101;
const ID_CONFIG: usize = 102;
const ID_LOG: usize = 103;
const ID_QUIT: usize = 104;
const ID_GITHUB: usize = 105;

const CLIENT_W: i32 = 880;
const CLIENT_H: i32 = 790;
const WIN_STYLE: u32 = WS_CAPTION.0 | WS_SYSMENU.0 | WS_MINIMIZEBOX.0 | WS_CLIPCHILDREN.0;

const BG_COLOR: u32 = 0x0012_1418;
const FIELD_COLOR: u32 = 0x0020_2329;
const TEXT_COLOR: u32 = 0x00F2_F4F7;
static BG_BRUSH: AtomicIsize = AtomicIsize::new(0);
static FIELD_BRUSH: AtomicIsize = AtomicIsize::new(0);

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
/// DPI of the display the launcher is built for (96 = 100%). All control coordinates are written
/// in these 96-DPI "design" units and scaled by `scale` at creation, so the window and its text
/// stay readable on high-DPI screens.
static DPI: AtomicU32 = AtomicU32::new(96);
/// The DPI-scaled UI font (0 = fall back to the stock GUI font).
static FONT: AtomicIsize = AtomicIsize::new(0);

/// Scales a design-unit coordinate / size to physical pixels for the current `DPI`.
fn scale(v: i32) -> i32 {
    (v as f64 * DPI.load(Ordering::Relaxed) as f64 / 96.0).round() as i32
}

/// Creates a UI font at 10 pt scaled for `dpi` (the process is per-monitor-DPI aware, so a stock
/// font would be unscaled and look tiny on a high-DPI display).
fn make_font(dpi: u32) -> HFONT {
    unsafe {
        CreateFontW(
            -(10 * dpi as i32 / 72), // negative height = character height in pixels
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        )
    }
}

#[derive(Default)]
struct Controls {
    inner_gap: HWND,
    outer_gap: HWND,
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
    indicator_size: HWND,
    border: HWND,
    border_color: HWND,
    border_width: HWND,
    fullscreen: HWND,
    media: HWND,
    hotkeys: HWND,
    autostart: HWND,
    focus_follows_mouse: HWND,
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
    let (x, y, w, h) = (scale(x), scale(y), scale(w), scale(h));
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
    let raw = FONT.load(Ordering::Relaxed);
    let font = if raw != 0 {
        HGDIOBJ(raw as *mut _)
    } else {
        GetStockObject(DEFAULT_GUI_FONT)
    };
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
            180,
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
        label(parent, "TILE-RS  /  SETTINGS", 20, 18);
        label(
            parent,
            "A focused workspace, tuned to the way you work.",
            20,
            43,
        );
        button(parent, "Check for updates", ID_UPDATES, 625, 22, 130);
        button(parent, "GitHub  ->", ID_GITHUB, 765, 22, 95);

        group(parent, "LAYOUT", 16, 78, 416, 230);
        c.layout[0] = radio(parent, "Spiral", 28, 108, true, true);
        c.layout[1] = radio(parent, "Master / stack", 28, 138, true, false);
        c.layout[2] = radio(parent, "Grid", 28, 168, true, false);
        c.layout[3] = radio(parent, "Columns", 28, 198, true, false);
        label(parent, "Minimum tile width", 225, 108);
        c.min_w = edit(parent, 365, 105, 55);
        label(parent, "Minimum tile height", 225, 138);
        c.min_h = edit(parent, 365, 135, 55);
        label(parent, "Tile limit (0 = auto)", 225, 168);
        c.max_tiles = edit(parent, 365, 165, 55);
        c.auto_overflow = check(
            parent,
            "Move overflow to next workspace",
            225,
            202,
            195,
            true,
        );

        group(parent, "SPACING & MOTION", 448, 78, 416, 230);
        label(parent, "Window gap", 462, 110);
        c.inner_gap = edit(parent, 575, 107, 55);
        label(parent, "Screen edge", 650, 110);
        c.outer_gap = edit(parent, 770, 107, 55);
        c.animations = check(parent, "Animate window movement", 462, 145, 220, true);
        label(parent, "Duration (ms)", 462, 178);
        c.anim_ms = edit(parent, 575, 175, 65);
        label(parent, "Motion curve", 660, 178);
        c.easing = combo(parent, &EASING_NAMES, 2, 755, 174, 95);
        label(parent, "Resize step (px)", 462, 213);
        c.resize_step = edit(parent, 575, 210, 65);

        group(parent, "WINDOW BEHAVIOUR", 16, 326, 416, 238);
        c.indicator = check(parent, "Show workspace switcher", 28, 356, 200, true);
        c.focus_follows_mouse = check(parent, "Focus follows pointer", 235, 356, 185, false);
        label(parent, "Fullscreen apps", 28, 392);
        c.fullscreen = combo(
            parent,
            &["Minimize on other workspaces", "Leave untouched"],
            0,
            168,
            389,
            252,
        );
        label(parent, "Photo / video windows", 28, 430);
        c.media = combo(parent, &["Float", "Tile"], 0, 168, 427, 252);
        label(parent, "Workspace dots", 28, 463);
        c.indicator_size = combo(parent, &["Small", "Medium", "Large"], 1, 168, 460, 130);
        c.border = check(parent, "Highlight focused window", 28, 492, 220, true);
        label(parent, "Border color", 28, 528);
        c.border_color = edit(parent, 120, 525, 90);
        label(parent, "Width", 250, 528);
        c.border_width = edit(parent, 300, 525, 55);

        group(
            parent,
            "HOTKEYS  /  EDIT AS action = key",
            448,
            326,
            416,
            225,
        );
        c.hotkeys = mk(parent, w!("EDIT"), "", S_HOTKEYS, 0, 462, 356, 388, 180);

        group(parent, "QUICK ACCESS", 16, 570, 848, 135);
        c.autostart = check(parent, "Start tile-rs when I sign in", 28, 603, 330, false);
        label(
            parent,
            "Application rules and advanced options are in config.toml.",
            28,
            640,
        );
        button(parent, "Open config", ID_CONFIG, 28, 666, 110);
        button(parent, "Open log", ID_LOG, 148, 666, 95);

        button(parent, "Save  &  apply", ID_SAVE, 16, 730, 160);
        button(parent, "Quit tile-rs", ID_QUIT, 748, 730, 116);
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
                ID_UPDATES => crate::update::check_and_notify(),
                ID_GITHUB => tray::open("https://github.com/ponslookspain/tile-rs", ""),
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
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut _);
            unsafe {
                let _ = SetTextColor(hdc, COLORREF(TEXT_COLOR));
                let _ = SetBkColor(hdc, COLORREF(BG_COLOR));
            }
            LRESULT(BG_BRUSH.load(Ordering::Relaxed))
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut _);
            unsafe {
                let _ = SetTextColor(hdc, COLORREF(TEXT_COLOR));
                let _ = SetBkColor(hdc, COLORREF(FIELD_COLOR));
            }
            LRESULT(FIELD_BRUSH.load(Ordering::Relaxed))
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
        let media = if combo_sel(c.media) == 1 {
            "tile"
        } else {
            "float"
        };
        let indicator_size = match combo_sel(c.indicator_size).min(2) {
            0 => IndicatorSize::Small,
            2 => IndicatorSize::Large,
            _ => IndicatorSize::Medium,
        };
        let num = |h: HWND| get_text(h).trim().to_string();
        let pairs: Vec<(&str, String)> = vec![
            ("layout", format!("\"{}\"", layout.name())),
            ("inner_gap", num(c.inner_gap)),
            ("outer_gap", num(c.outer_gap)),
            ("animations", is_checked(c.animations).to_string()),
            ("animation_ms", num(c.anim_ms)),
            ("easing", format!("\"{easing}\"")),
            ("resize_step", num(c.resize_step)),
            ("min_tile_width", num(c.min_w)),
            ("min_tile_height", num(c.min_h)),
            ("max_tiles", num(c.max_tiles)),
            ("auto_overflow", is_checked(c.auto_overflow).to_string()),
            ("workspace_indicator", is_checked(c.indicator).to_string()),
            (
                "workspace_indicator_size",
                format!("\"{}\"", indicator_size.name()),
            ),
            (
                "focus_follows_mouse",
                is_checked(c.focus_follows_mouse).to_string(),
            ),
            ("border", is_checked(c.border).to_string()),
            ("border_color", format!("\"{}\"", num(c.border_color))),
            ("border_width", num(c.border_width)),
            ("fullscreen_windows", format!("\"{fullscreen}\"")),
            ("media_windows", format!("\"{media}\"")),
        ];
        (pairs, get_text(c.hotkeys), is_checked(c.autostart))
    });
    let path = config::config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            crate::notify("tile-rs", &format!("Could not read settings: {err}"));
            return;
        }
    };
    let new = edit_hotkeys(&config::edit_top(&text, &pairs), &hotkeys);
    if let Err(err) = config::parse(&new) {
        crate::notify("tile-rs", &format!("Settings were not saved: {err}"));
        return;
    }
    if let Err(err) = std::fs::write(&path, new) {
        crate::notify("tile-rs", &format!("Could not save settings: {err}"));
        return;
    }
    crate::set_autostart(autostart);
    crate::reload_config();
    crate::notify("tile-rs", "Settings saved.");
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
}

/// Replaces the `[hotkeys]` section with `lines` (raw `action = key` lines), leaving the rest.
/// Each value is normalised so the result is always valid TOML: a bare key is quoted, an already
/// quoted string or a `[...]` list is kept as it is.
fn edit_hotkeys(text: &str, lines: &str) -> String {
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
    s
}

fn populate() {
    let c = config::current();
    C.with(|cc| {
        let cc = cc.borrow();
        set_text(cc.inner_gap, &c.inner_gap.to_string());
        set_text(cc.outer_gap, &c.outer_gap.to_string());
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
        let indicator_size = match c.workspace_indicator_size {
            IndicatorSize::Small => 0,
            IndicatorSize::Medium => 1,
            IndicatorSize::Large => 2,
        };
        sm(cc.indicator_size, CB_SETCURSEL, indicator_size, 0);
        set_check(cc.focus_follows_mouse, c.focus_follows_mouse);
        set_check(cc.border, c.border);
        set_check(cc.autostart, crate::autostart_enabled());
        sm(
            cc.fullscreen,
            CB_SETCURSEL,
            usize::from(c.fullscreen_windows == FullscreenWindows::Ignore),
            0,
        );
        sm(
            cc.media,
            CB_SETCURSEL,
            usize::from(c.media_windows == MediaWindows::Tile),
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
        let bg = CreateSolidBrush(COLORREF(BG_COLOR));
        let field = CreateSolidBrush(COLORREF(FIELD_COLOR));
        BG_BRUSH.store(bg.0 as isize, Ordering::Relaxed);
        FIELD_BRUSH.store(field.0 as isize, Ordering::Relaxed);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hbrBackground: bg,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        // build the launcher for the display's DPI before the controls are created (WM_CREATE)
        let dpi = GetDpiForSystem().max(96);
        DPI.store(dpi, Ordering::Relaxed);
        FONT.store(make_font(dpi).0 as isize, Ordering::Relaxed);
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: scale(CLIENT_W),
            bottom: scale(CLIENT_H),
        };
        let _ = AdjustWindowRectExForDpi(
            &mut rc,
            WINDOW_STYLE(WIN_STYLE),
            false,
            WINDOW_EX_STYLE(0),
            dpi,
        );
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - w) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - h) / 2;
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("tile-rs  /  Settings"),
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
    let font = FONT.swap(0, Ordering::Relaxed);
    if font != 0 {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(font as *mut _));
        }
    }
    for brush in [&BG_BRUSH, &FIELD_BRUSH] {
        let raw = brush.swap(0, Ordering::Relaxed);
        if raw != 0 {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(raw as *mut _));
            }
        }
    }
}
