//! Slint settings launcher. The UI runs as a short-lived child process so its framework event loop
//! cannot block tile-rs' Win32 loop, WinEvent hooks, tray messages, or animation timers.

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::config;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, SetEvent, WaitForSingleObject,
};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE,
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

    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    if std::env::args().any(|arg| arg == "--updates") {
        ui.set_page(6);
    }
    wire_updates(&ui);
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
    ui.on_save({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                save(&ui);
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
                        ui.set_page(6);
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
    ui.set_snapshot_animations(cfg.snapshot_animations);
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
    ui.set_hotkeys(hotkeys_text().into());
    ui.set_status(format!("Настройки загружены · версия {}", env!("CARGO_PKG_VERSION")).into());
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
            ui.set_update_message("Получаем информацию о последнем релизе…".into());
            let weak = ui.as_weak();
            let release = release.clone();
            if let Err(err) = thread::Builder::new().name("launcher-update-check".into()).spawn(move || {
                let result = crate::update::check_latest();
                let _ = weak.upgrade_in_event_loop(move |ui| match result {
                    Ok(Some(found)) => {
                        ui.set_update_state(2);
                        ui.set_update_version(found.version.clone().into());
                        ui.set_release_notes(found.notes.clone().into());
                        ui.set_update_message("После загрузки откроется установщик, затем tile-rs перезапустится. Сохраните изменения настроек перед установкой.".into());
                        *release.lock().unwrap() = Some(found);
                    }
                    Ok(None) => {
                        ui.set_update_state(3);
                        ui.set_update_message("Обновлений нет. Можно продолжать работу.".into());
                        ui.set_release_notes("".into());
                        *release.lock().unwrap() = None;
                    }
                    Err(err) => {
                        ui.set_update_state(5);
                        ui.set_update_message(format!("Проверка не удалась: {err}. Нажмите Check update, чтобы повторить.").into());
                    }
                });
            }) {
                ui.set_update_state(5);
                ui.set_update_message(format!("Не удалось запустить проверку: {err}").into());
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
            ui.set_update_message("Загружаем установщик и проверяем SHA-256…".into());
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
                        ui.set_update_message(format!("Установка не началась: {err}. Повторите проверку или откройте релизы.").into());
                    }
                });
            }) {
                ui.set_update_state(5);
                ui.set_update_message(format!("Не удалось запустить загрузку: {err}").into());
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
            let values = specs
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("{name} = [{values}]\n"));
        }
    }
    out
}

fn save(ui: &MainWindow) {
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
    let pairs: Vec<(&str, String)> = vec![
        ("layout", format!("\"{}\"", layout.name())),
        ("inner_gap", ui.get_inner_gap().to_string()),
        ("outer_gap", ui.get_outer_gap().to_string()),
        ("animations", ui.get_animations().to_string()),
        (
            "snapshot_animations",
            ui.get_snapshot_animations().to_string(),
        ),
        ("animation_ms", ui.get_animation_ms().to_string()),
        ("easing", format!("\"{easing}\"")),
        ("resize_step", ui.get_resize_step().to_string()),
        ("min_tile_width", ui.get_min_tile_width().to_string()),
        ("min_tile_height", ui.get_min_tile_height().to_string()),
        ("max_tiles", ui.get_max_tiles().to_string()),
        ("auto_overflow", ui.get_auto_overflow().to_string()),
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
        ("border_width", ui.get_border_width().to_string()),
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

    let path = config::config_path();
    let current = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            ui.set_status(format!("Не удалось прочитать конфиг: {err}").into());
            return;
        }
    };
    let updated = edit_hotkeys(
        &config::edit_top(&current, &pairs),
        ui.get_hotkeys().as_ref(),
    );
    if let Err(err) = config::parse(&updated) {
        ui.set_status(format!("Проверьте настройки: {err}").into());
        return;
    }
    if let Err(err) = config::save_atomic(&path, &updated) {
        ui.set_status(format!("Не удалось сохранить настройки: {err}").into());
        return;
    }
    if !crate::set_autostart(ui.get_autostart()) {
        ui.set_status("Настройки сохранены, но не удалось изменить автозапуск".into());
    } else if crate::tray::request_manager_reload() {
        ui.set_status("Настройки сохранены и применены".into());
    } else {
        ui.set_status("Настройки сохранены; применятся при следующем запуске".into());
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
