//! Slint settings launcher. The UI runs as a short-lived child process so its framework event loop
//! cannot block tile-rs' Win32 loop, WinEvent hooks, tray messages, or animation timers.

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::config;
use crate::launcher_model::{self, Capture};
use slint::{ModelRc, SharedString, VecModel};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MessageBoxW, SetForegroundWindow, ShowWindow, IDNO, IDYES, MB_ICONQUESTION,
    MB_YESNOCANCEL, MESSAGEBOX_RESULT, SW_RESTORE,
};

slint::include_modules!();

const LAUNCHER_TITLE: windows::core::PCWSTR = w!("tile-rs Settings");

/// The settings UI owns its event loop in a separate process; nothing is initialized in the manager.
pub fn create() -> bool {
    true
}

/// Start the UI process, or bring the existing launcher to the foreground.
pub fn show() {
    show_page(false);
}

pub fn show_updates() {
    show_page(true);
}

fn show_page(updates: bool) {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            crate::notify(
                "tile-rs",
                &format!("Cannot locate launcher executable: {err}"),
            );
            return;
        }
    };
    let mut command = Command::new(exe);
    command.arg("--launcher-ui");
    if updates {
        command.arg("--updates");
    }
    if let Err(err) = command.spawn() {
        crate::notify("tile-rs", &format!("Cannot open settings: {err}"));
    }
}

/// The UI process owns and destroys its own Slint window when it exits.
pub fn destroy() {}

/// Entry point for the isolated settings UI process.
pub fn run() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    let mutex =
        match unsafe { CreateMutexW(None, true, w!("Local\\tile-rs-launcher-single-instance")) } {
            Ok(handle) => handle,
            Err(err) => {
                crate::notify(
                    "tile-rs settings",
                    &format!("Could not start launcher: {err}"),
                );
                return;
            }
        };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        activate_existing();
        if !mutex.is_invalid() {
            unsafe {
                let _ = CloseHandle(mutex);
            }
        }
        return;
    }

    config::ensure_default();
    let cfg = match config::load() {
        Ok(cfg) => cfg,
        Err(err) => {
            crate::notify("tile-rs settings", &format!("Could not load config: {err}"));
            if !mutex.is_invalid() {
                unsafe {
                    let _ = CloseHandle(mutex);
                }
            }
            return;
        }
    };
    config::set(cfg.clone());

    let ui = match MainWindow::new() {
        Ok(ui) => ui,
        Err(err) => {
            crate::notify(
                "tile-rs settings",
                &format!("Could not create launcher: {err}"),
            );
            if !mutex.is_invalid() {
                unsafe {
                    let _ = CloseHandle(mutex);
                }
            }
            return;
        }
    };
    populate(&ui, &cfg);
    ui.global::<ColorParser>()
        .on_parse(|text| parse_hex_color(text.as_str()));

    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    if std::env::args().any(|arg| arg == "--updates") {
        ui.set_page(7);
    }
    // `--page N` opens a given page (used for design previews and screenshots).
    let args: Vec<String> = std::env::args().collect();
    if let Some(page) = args
        .windows(2)
        .find(|pair| pair[0] == "--page")
        .and_then(|pair| pair[1].parse::<i32>().ok())
    {
        ui.set_page(page.clamp(0, 7));
    }
    wire_updates(&ui);
    let _autosave = wire_editors(&ui);
    // The launcher is light-only: keep the caption light even when Windows uses dark mode.
    slint::Timer::single_shot(Duration::from_millis(60), light_caption);
    if std::env::args().any(|arg| arg == "--preview-dirty") {
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(700), move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_inner_gap("12".into());
            }
        });
    }
    ui.on_open_github(|| {
        crate::tray::open("https://github.com/ponslookspain/tile-rs", "");
    });
    ui.on_open_config(|| {
        config::ensure_default();
        crate::tray::open(
            "notepad.exe",
            &format!("\"{}\"", config::config_path().display()),
        );
    });
    ui.on_open_log(|| {
        crate::tray::open(
            "notepad.exe",
            &format!("\"{}\"", crate::log_file().display()),
        );
    });
    // Property changes made while the window is being filled are not user edits.
    slint::Timer::single_shot(Duration::from_millis(400), {
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_dirty(false);
            }
        }
    });
    ui.on_quit_manager(|| {
        crate::tray::request_manager_quit();
        slint::quit_event_loop().ok();
    });

    // Named event routes update notifications into an already open single-instance UI.
    let updates_event =
        unsafe { CreateEventW(None, false, false, w!("Local\\tile-rs-launcher-updates")) }.ok();
    let routing_timer = slint::Timer::default();
    if let Some(event) = updates_event {
        let weak = ui.as_weak();
        routing_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(250),
            move || {
                if unsafe { WaitForSingleObject(event, 0) } == WAIT_OBJECT_0 {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_page(7);
                        ui.invoke_check_updates();
                    }
                }
            },
        );
    }
    if let Err(err) = ui.run() {
        crate::log(format!("launcher event loop failed: {err}"));
    }
    routing_timer.stop();
    if let Some(event) = updates_event {
        unsafe {
            let _ = CloseHandle(event);
        }
    }
    if !mutex.is_invalid() {
        unsafe {
            let _ = CloseHandle(mutex);
        }
    }
}

fn light_caption() {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
    let Ok(hwnd) = (unsafe { FindWindowW(PCWSTR::null(), LAUNCHER_TITLE) }) else {
        return;
    };
    let dark: i32 = 0; // FALSE
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

fn activate_existing() {
    if std::env::args().any(|arg| arg == "--updates") {
        if let Ok(event) =
            unsafe { CreateEventW(None, false, false, w!("Local\\tile-rs-launcher-updates")) }
        {
            unsafe {
                let _ = SetEvent(event);
                let _ = CloseHandle(event);
            }
        }
    }
    // A second tray click can arrive before Slint has registered the top-level window.
    for _ in 0..20 {
        let hwnd = unsafe { FindWindowW(PCWSTR::null(), LAUNCHER_TITLE) }.unwrap_or_default();
        if !hwnd.0.is_null() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn populate(ui: &MainWindow, cfg: &config::Config) {
    use crate::config::{FullscreenWindows, IndicatorSize, Layout, MediaWindows};

    ui.set_layout_index(match cfg.layout {
        Layout::Spiral => 0,
        Layout::MasterStack => 1,
        Layout::Grid => 2,
        Layout::Columns => 3,
    });
    ui.set_indicator_size(match cfg.workspace_indicator_size {
        IndicatorSize::Small => 0,
        IndicatorSize::Medium => 1,
        IndicatorSize::Large => 2,
    });
    ui.set_animations(cfg.animations);
    ui.set_auto_overflow(cfg.auto_overflow);
    ui.set_show_indicator(cfg.workspace_indicator);
    ui.set_focus_follows_mouse(cfg.focus_follows_mouse);
    ui.set_highlight_window(cfg.border);
    ui.set_autostart(crate::autostart_enabled());
    ui.set_inner_gap(cfg.inner_gap.to_string().into());
    ui.set_outer_gap(cfg.outer_gap.to_string().into());
    ui.set_animation_ms(cfg.animation_ms.to_string().into());
    ui.set_easing(easing_name(cfg.easing).into());
    ui.set_resize_step(cfg.resize_step.to_string().into());
    ui.set_min_tile_width(cfg.min_tile_width.to_string().into());
    ui.set_min_tile_height(cfg.min_tile_height.to_string().into());
    ui.set_max_tiles(cfg.max_tiles.to_string().into());
    ui.set_accent_color(
        format!(
            "#{:02x}{:02x}{:02x}",
            cfg.accent_color & 0xff,
            (cfg.accent_color >> 8) & 0xff,
            (cfg.accent_color >> 16) & 0xff
        )
        .into(),
    );
    ui.set_border_color(
        format!(
            "#{:02x}{:02x}{:02x}",
            cfg.border_color & 0xff,
            (cfg.border_color >> 8) & 0xff,
            (cfg.border_color >> 16) & 0xff
        )
        .into(),
    );
    ui.set_border_width(cfg.border_width.to_string().into());
    ui.set_fullscreen_windows(i32::from(
        cfg.fullscreen_windows == FullscreenWindows::Ignore,
    ));
    ui.set_media_windows(i32::from(cfg.media_windows == MediaWindows::Tile));
    ui.set_smart_gaps(cfg.smart_gaps);
    ui.set_master_count(cfg.master_count.to_string().into());
    ui.set_status("".into()); // empty: the header shows the page subtitle
}

/// UI state stays on Slint's thread; only network and installer work run on workers.
fn wire_updates(ui: &MainWindow) {
    let release = Arc::new(Mutex::new(None::<crate::update::Release>));
    ui.on_check_updates({
        let weak = ui.as_weak();
        let release = release.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return; };
            if matches!(ui.get_update_state(), 1 | 4) { return; }
            ui.set_update_state(1);
            ui.set_update_message("Looking up the latest release…".into());
            let weak = ui.as_weak();
            let release = release.clone();
            if let Err(err) = thread::Builder::new().name("launcher-update-check".into()).spawn(move || {
                let result = crate::update::check_latest();
                let _ = weak.upgrade_in_event_loop(move |ui| match result {
                    Ok(Some(found)) => {
                        ui.set_update_state(2);
                        ui.set_update_version(found.version.clone().into());
                        ui.set_release_notes(found.notes.clone().into());
                        ui.set_update_message("The installer opens after the download, then tile-rs restarts.".into());
                        *release.lock().unwrap() = Some(found);
                    }
                    Ok(None) => {
                        ui.set_update_state(3);
                        ui.set_update_message("No updates. You are all set.".into());
                        ui.set_release_notes("".into());
                        *release.lock().unwrap() = None;
                    }
                    Err(err) => {
                        ui.set_update_state(5);
                        ui.set_update_message(format!("The check failed: {err}. Press Check to try again.").into());
                    }
                });
            }) {
                ui.set_update_state(5);
                ui.set_update_message(format!("Could not start the check: {err}").into());
            }
        }
    });
    ui.on_install_update({
        let weak = ui.as_weak();
        let release = release.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return; };
            if ui.get_update_state() != 2 { return; }
            let Some(found) = release.lock().unwrap().clone() else { return; };
            ui.set_update_state(4);
            ui.set_update_progress(0);
            ui.set_update_message("Downloading the installer and checking its SHA-256…".into());
            let weak = ui.as_weak();
            let worker_ui = weak.clone();
            if let Err(err) = thread::Builder::new().name("launcher-update-install".into()).spawn(move || {
                let progress_ui = worker_ui.clone();
                let result = crate::update::install(&found, move |percent| {
                    let _ = progress_ui.upgrade_in_event_loop(move |ui| ui.set_update_progress(i32::from(percent)));
                });
                let _ = worker_ui.upgrade_in_event_loop(move |ui| match result {
                    Ok(()) => { let _ = slint::quit_event_loop(); }
                    Err(err) => {
                        ui.set_update_state(5);
                        ui.set_update_message(format!("The install did not start: {err}. Check again or open the releases page.").into());
                    }
                });
            }) {
                ui.set_update_state(5);
                ui.set_update_message(format!("Could not start the download: {err}").into());
            }
        }
    });
    ui.on_open_release({
        let release = release.clone();
        move || {
            let url = release
                .lock()
                .unwrap()
                .as_ref()
                .map(|r| r.url.clone())
                .unwrap_or_else(|| {
                    "https://github.com/ponslookspain/tile-rs/releases/latest".into()
                });
            crate::tray::open(&url, "");
        }
    });
    // Populate the update badge as soon as the launcher opens without blocking its event loop.
    ui.invoke_check_updates();
}

fn easing_name(easing: crate::geometry::Easing) -> &'static str {
    use crate::geometry::Easing;
    match easing {
        Easing::Linear => "linear",
        Easing::OutQuad => "out_quad",
        Easing::OutCubic => "out_cubic",
        Easing::OutQuart => "out_quart",
        Easing::InOutCubic => "in_out_cubic",
    }
}

/// "#RRGGBB" -> colour for the swatches; anything else is transparent.
fn parse_hex_color(text: &str) -> slint::Color {
    let hex = text.trim().trim_start_matches('#');
    match (hex.len(), u32::from_str_radix(hex, 16)) {
        (6, Ok(v)) => slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8),
        _ => slint::Color::from_argb_u8(0, 0, 0, 0),
    }
}

/// Where the "welcome" card remembers that it was shown.
fn welcome_marker() -> std::path::PathBuf {
    config::config_path().with_file_name("welcome-shown")
}

/// State of the hotkey and rule editors, which live outside the Slint properties.
struct Editor {
    rows: Vec<launcher_model::Row>,
    rules: Vec<config::Rule>,
    rules_dirty: bool,
    /// (process, class, title) of the running windows offered in the rule picker.
    apps: Vec<(String, String, String)>,
    /// (action, keys) pairs Windows refused to register at the manager's last (re)load.
    refused: Vec<(String, String)>,
    /// Autosave ticks left before the manager's answer about refused keys is read.
    recheck: u8,
    /// A change was seen on the previous autosave tick (saves wait for a short pause).
    pending: bool,
}

fn read_refused() -> Vec<(String, String)> {
    std::fs::read_to_string(config::hotkey_failures_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(action, keys)| (action.to_string(), keys.to_string()))
        .collect()
}

type Shared = Rc<RefCell<Editor>>;

fn push_rows(ui: &MainWindow, editor: &Editor) {
    let flags = launcher_model::conflicts(&editor.rows);
    let rows: Vec<HotkeyRowData> = editor
        .rows
        .iter()
        .zip(flags)
        .map(|(row, conflict)| HotkeyRowData {
            action: row.action.clone().into(),
            label: row.label.clone().into(),
            group: row.group.clone().into(),
            keys: ModelRc::new(VecModel::from(
                row.keys
                    .iter()
                    .map(|k| SharedString::from(launcher_model::display_spec(k)))
                    .collect::<Vec<_>>(),
            )),
            conflict,
            refused: editor
                .refused
                .iter()
                .any(|(a, k)| *a == row.action && row.keys.contains(k)),
        })
        .collect();
    ui.set_hotkeys_bound(editor.rows.iter().filter(|r| !r.keys.is_empty()).count() as i32);
    ui.set_hotkey_rows(ModelRc::new(VecModel::from(rows)));
    let items: Vec<WelcomeItem> = launcher_model::welcome_items(&editor.rows)
        .into_iter()
        .map(|(keys, what)| WelcomeItem {
            keys: keys.into(),
            what: what.into(),
        })
        .collect();
    ui.set_welcome_items(ModelRc::new(VecModel::from(items)));
    ui.set_master_hint(launcher_model::master_hint(&editor.rows).into());
}

fn push_rules(ui: &MainWindow, editor: &Editor) {
    let rules: Vec<SharedString> = editor
        .rules
        .iter()
        .map(|rule| launcher_model::rule_label(rule).into())
        .collect();
    ui.set_rule_rows(ModelRc::new(VecModel::from(rules)));
}

fn push_apps(ui: &MainWindow, editor: &Editor) {
    let choices: Vec<SharedString> = editor
        .apps
        .iter()
        .map(|(process, _, title)| {
            let title: String = title.chars().take(44).collect();
            format!("{title}  ·  {process}.exe").into()
        })
        .collect();
    ui.set_app_choices(ModelRc::new(VecModel::from(choices)));
    ui.set_rule_app_index(0);
}

/// Hotkey editor, application rules and the welcome card.
fn wire_editors(ui: &MainWindow) -> slint::Timer {
    let editor: Shared = Rc::new(RefCell::new(Editor {
        rows: launcher_model::rows_from_bindings(&config::current().bindings),
        rules: config::current().rules,
        rules_dirty: false,
        apps: crate::window::running_apps(),
        refused: read_refused(),
        recheck: 0,
        pending: false,
    }));
    {
        let ed = editor.borrow();
        push_rows(ui, &ed);
        push_rules(ui, &ed);
        push_apps(ui, &ed);
    }
    ui.set_show_welcome(!welcome_marker().exists());
    ui.on_dismiss_welcome({
        let weak = ui.as_weak();
        move || {
            let _ = std::fs::write(welcome_marker(), "1");
            if let Some(ui) = weak.upgrade() {
                ui.set_show_welcome(false);
            }
        }
    });

    ui.on_key_captured({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move |index, append, text, alt, ctrl, shift, meta| {
            let Some(ui) = weak.upgrade() else { return };
            let mut ed = editor.borrow_mut();
            let Some(row) = usize::try_from(index).ok().and_then(|i| ed.rows.get_mut(i)) else {
                ui.set_recording(-1);
                return;
            };
            let text = held_key().map_or_else(|| text.to_string(), String::from);
            match launcher_model::capture_key(&text, alt, ctrl, shift, meta) {
                Capture::Wait => {}
                Capture::Cancel => ui.set_recording(-1),
                Capture::Clear => {
                    row.keys.clear();
                    ui.set_recording(-1);
                    push_rows(&ui, &ed);
                    ui.set_dirty(true);
                }
                Capture::Reject(reason) => ui.set_status(reason.into()),
                Capture::Spec(spec) => {
                    if !append {
                        row.keys.clear();
                    }
                    if !row.keys.contains(&spec) {
                        row.keys.push(spec);
                    }
                    ui.set_recording(-1);
                    push_rows(&ui, &ed);
                    ui.set_dirty(true);
                    if launcher_model::conflicts(&ed.rows).iter().any(|c| *c) {
                        ui.set_status(
                            "These keys are already used by another action (marked ⚠)".into(),
                        );
                    }
                }
            }
        }
    });
    ui.on_clear_keys({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            let mut ed = editor.borrow_mut();
            if let Some(row) = usize::try_from(index).ok().and_then(|i| ed.rows.get_mut(i)) {
                row.keys.clear();
            }
            ui.set_recording(-1);
            push_rows(&ui, &ed);
            ui.set_dirty(true);
        }
    });
    ui.on_reset_hotkeys({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut ed = editor.borrow_mut();
            ed.rows = launcher_model::default_rows();
            ui.set_recording(-1);
            push_rows(&ui, &ed);
            ui.set_dirty(true);
            ui.set_status("Hotkeys reset to the defaults".into());
        }
    });

    ui.on_refresh_apps({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut ed = editor.borrow_mut();
            ed.apps = crate::window::running_apps();
            push_apps(&ui, &ed);
        }
    });
    ui.on_add_rule({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut ed = editor.borrow_mut();
            let Some((process, _, _)) = usize::try_from(ui.get_rule_app_index())
                .ok()
                .and_then(|i| ed.apps.get(i).cloned())
            else {
                return;
            };
            let kind = ui.get_rule_kind_index().clamp(0, 9);
            let rule = config::Rule {
                process: Some(process),
                action: (kind == 0).then(|| "float".to_string()),
                workspace: (kind > 0).then_some(kind as u8),
                ..config::Rule::default()
            };
            let same = |r: &config::Rule| {
                r.process == rule.process
                    && r.class.is_none()
                    && r.title.is_none()
                    && r.action == rule.action
                    && r.workspace == rule.workspace
            };
            if ed.rules.iter().any(same) {
                ui.set_status("This rule already exists".into());
                return;
            }
            ed.rules.push(rule);
            ed.rules_dirty = true;
            push_rules(&ui, &ed);
            ui.set_dirty(true);
            ui.set_status("Rule added".into());
        }
    });
    ui.on_delete_rule({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move |index| {
            let Some(ui) = weak.upgrade() else { return };
            let mut ed = editor.borrow_mut();
            if let Ok(i) = usize::try_from(index) {
                if i < ed.rules.len() {
                    ed.rules.remove(i);
                    ed.rules_dirty = true;
                    ui.set_dirty(true);
                }
            }
            push_rules(&ui, &ed);
        }
    });

    ui.on_save({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move || {
            if let Some(ui) = weak.upgrade() {
                save(&ui, &mut editor.borrow_mut());
            }
        }
    });

    ui.window().on_close_requested({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move || {
            use slint::CloseRequestResponse::{HideWindow, KeepWindowShown};
            let Some(ui) = weak.upgrade() else { return HideWindow };
            if ui.get_dirty() {
                save(&ui, &mut editor.borrow_mut());
            }
            if !ui.get_dirty() {
                return HideWindow;
            }
            match ask_save_on_close() {
                IDYES => {
                    save(&ui, &mut editor.borrow_mut());
                    // a failed save keeps the window open with its status message
                    if ui.get_dirty() { KeepWindowShown } else { HideWindow }
                }
                IDNO => HideWindow,
                _ => KeepWindowShown,
            }
        }
    });

    // Changes are applied by themselves a moment after the last edit; there is no Save button.
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(400),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut ed = editor.borrow_mut();
            if ed.recheck > 0 {
                ed.recheck -= 1;
                if ed.recheck == 0 {
                    ed.refused = read_refused();
                    push_rows(&ui, &ed);
                    if !ed.refused.is_empty() {
                        ui.set_status(
                            format!(
                                "Windows refused {} key combination(s). They are marked ⚠, pick others",
                                ed.refused.len()
                            )
                            .into(),
                        );
                    }
                }
            }
            if !ui.get_dirty() || ui.get_recording() >= 0 {
                ed.pending = false;
            } else if ed.pending {
                ed.pending = false;
                save(&ui, &mut ed);
            } else {
                ed.pending = true;
            }
        },
    );
    timer
}

/// The letter, digit or punctuation key physically held right now, independent of the keyboard
/// layout and of Shift; `None` for arrows, F-keys and the like, which Slint reports reliably.
fn held_key() -> Option<char> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    (0x30u16..=0xbe)
        .filter(|vk| unsafe { GetAsyncKeyState(i32::from(*vk)) } < 0)
        .find_map(launcher_model::vk_char)
}

fn ask_save_on_close() -> MESSAGEBOX_RESULT {
    let owner = unsafe { FindWindowW(PCWSTR::null(), LAUNCHER_TITLE) }.ok();
    unsafe {
        MessageBoxW(
            owner,
            w!("Some changes could not be saved. Try saving them again before closing?"),
            LAUNCHER_TITLE,
            MB_YESNOCANCEL | MB_ICONQUESTION,
        )
    }
}

fn save(ui: &MainWindow, editor: &mut Editor) {
    use crate::config::{IndicatorSize, Layout};

    let layout = match ui.get_layout_index() {
        1 => Layout::MasterStack,
        2 => Layout::Grid,
        3 => Layout::Columns,
        _ => Layout::Spiral,
    };
    let indicator = match ui.get_indicator_size() {
        0 => IndicatorSize::Small,
        2 => IndicatorSize::Large,
        _ => IndicatorSize::Medium,
    };
    let easing = ui.get_easing().to_string();
    let border_color = ui.get_border_color().to_string();
    let accent_color = ui.get_accent_color().to_string();
    let mut bad: Option<String> = None;
    let mut num = |label: &str, text: SharedString, min: i64, max: i64| -> String {
        match text.trim().parse::<i64>() {
            Ok(v) if (min..=max).contains(&v) => v.to_string(),
            _ => {
                bad.get_or_insert(format!("{label}: enter a number from {min} to {max}"));
                String::new()
            }
        }
    };
    let pairs: Vec<(&str, String)> = vec![
        ("layout", format!("\"{}\"", layout.name())),
        ("inner_gap", num("Between windows", ui.get_inner_gap(), 0, 200)),
        ("outer_gap", num("From screen edges", ui.get_outer_gap(), 0, 200)),
        ("animations", ui.get_animations().to_string()),
        ("animation_ms", num("Animation length", ui.get_animation_ms(), 0, 1000)),
        ("easing", format!("\"{easing}\"")),
        ("resize_step", num("Resize step", ui.get_resize_step(), 1, 500)),
        ("min_tile_width", num("Minimum width", ui.get_min_tile_width(), 100, 4000)),
        ("min_tile_height", num("Minimum height", ui.get_min_tile_height(), 100, 4000)),
        ("max_tiles", num("Windows per workspace", ui.get_max_tiles(), 0, 20)),
        ("auto_overflow", ui.get_auto_overflow().to_string()),
        ("smart_gaps", ui.get_smart_gaps().to_string()),
        ("master_count", num("Windows in the main column", ui.get_master_count(), 1, 8)),
        ("workspace_indicator", ui.get_show_indicator().to_string()),
        (
            "workspace_indicator_size",
            format!("\"{}\"", indicator.name()),
        ),
        (
            "focus_follows_mouse",
            ui.get_focus_follows_mouse().to_string(),
        ),
        ("border", ui.get_highlight_window().to_string()),
        ("border_color", format!("\"{border_color}\"")),
        ("accent_color", format!("\"{accent_color}\"")),
        ("border_width", num("Border width", ui.get_border_width(), 0, 20)),
        (
            "fullscreen_windows",
            format!(
                "\"{}\"",
                if ui.get_fullscreen_windows() == 1 {
                    "ignore"
                } else {
                    "minimize"
                }
            ),
        ),
        (
            "media_windows",
            format!(
                "\"{}\"",
                if ui.get_media_windows() == 1 {
                    "tile"
                } else {
                    "float"
                }
            ),
        ),
    ];

    if let Some(message) = bad {
        ui.set_status(message.into());
        return;
    }
    if launcher_model::conflicts(&editor.rows).iter().any(|c| *c) {
        ui.set_status(
            "Fix the keys marked ⚠: one combination cannot be used twice".into(),
        );
        return;
    }
    let path = config::config_path();
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            ui.set_status(format!("Could not read the config: {err}").into());
            return;
        }
    };
    let mut updated = edit_hotkeys(
        &config::edit_top(&current, &pairs),
        &launcher_model::rows_to_lines(&editor.rows),
    );
    if editor.rules_dirty {
        updated = launcher_model::edit_rules(&updated, &editor.rules);
    }
    if let Err(err) = config::parse(&updated) {
        ui.set_status(format!("Check the settings: {err}").into());
        return;
    }
    if let Err(err) = config::save_atomic(&path, &updated) {
        ui.set_status(format!("Could not save the settings: {err}").into());
        return;
    }
    editor.rules_dirty = false;
    ui.set_dirty(false);
    if !crate::set_autostart(ui.get_autostart()) {
        ui.set_status("Saved, but starting with Windows could not be changed".into());
    } else if crate::tray::request_manager_reload() {
        editor.recheck = 2; // then the manager's answer about refused keys is read
        ui.set_status("Settings applied".into());
    } else {
        ui.set_status("Saved; tile-rs is not running, so they apply at its next start".into());
    }
}

/// Replaces the `[hotkeys]` section, leaving all unrelated config text untouched.
fn edit_hotkeys(text: &str, lines: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut hot: Vec<String> = Vec::new();
    for line in lines.lines() {
        let value = line.trim();
        if value.is_empty() || value.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = value.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            let value = if value.starts_with('[') || value.starts_with('"') {
                value.to_string()
            } else {
                format!("\"{}\"", value.trim_matches('"'))
            };
            hot.push(format!("{key} = {value}"));
        }
    }
    let mut out = Vec::new();
    let mut in_hotkeys = false;
    let mut found = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_hotkeys = trimmed == "[hotkeys]";
            found |= in_hotkeys;
            out.push(line.to_string());
            if in_hotkeys {
                out.extend(hot.iter().cloned());
            }
        } else if !in_hotkeys {
            out.push(line.to_string());
        }
    }
    if !found {
        out.push(String::new());
        out.push("[hotkeys]".into());
        out.extend(hot);
    }
    let mut output = out.join(nl);
    output.push_str(nl);
    output
}
