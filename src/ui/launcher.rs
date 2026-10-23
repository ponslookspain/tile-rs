//! Slint settings launcher. The UI runs as a short-lived child process so its framework event loop
//! cannot block tile-rs' Win32 loop, WinEvent hooks, tray messages, or animation timers.

use std::cell::RefCell;
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::config;
use crate::ui::launcher_model::{self, Capture};
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
    FindWindowW, MessageBoxW, SetForegroundWindow, ShowWindowAsync, IDYES, MB_DEFBUTTON2,
    MB_ICONQUESTION, MB_YESNO, MESSAGEBOX_RESULT, SW_RESTORE,
};

slint::include_modules!();

const LAUNCHER_TITLE: windows::core::PCWSTR = w!("tile-rs Settings");

/// The settings UI owns its event loop in a separate process; nothing is initialized in the manager.
pub fn create() -> bool {
    true
}

/// Start the UI process, or bring the existing launcher to the foreground.
pub fn show() {
    show_page(&[]);
}

pub fn show_updates() {
    show_page(&["--updates"]);
}

/// The settings on the Hotkeys page. An already open settings window is only brought forward.
pub fn show_hotkeys() {
    show_page(&["--page", "4"]);
}

fn show_page(args: &[&str]) {
    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            crate::platform::dialogs::notify(
                "tile-rs",
                &format!("Cannot locate launcher executable: {err}"),
            );
            return;
        }
    };
    let mut command = Command::new(exe);
    command.arg("--launcher-ui").args(args);
    if let Err(err) = command.spawn() {
        crate::platform::dialogs::notify("tile-rs", &format!("Cannot open settings: {err}"));
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
                crate::platform::dialogs::notify(
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
            crate::platform::dialogs::notify(
                "tile-rs settings",
                &format!("Could not load config: {err}"),
            );
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
            crate::platform::dialogs::notify(
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
        // pages 1-3 now live under "More options" on General, page 6 above the updates
        match page.clamp(0, 7) {
            1..=3 => {
                ui.set_page(0);
                ui.set_show_more(true);
            }
            6 => ui.set_page(7),
            page => ui.set_page(page),
        }
    }
    wire_updates(&ui);
    let _autosave = wire_editors(&ui);
    // The launcher is light-only: keep the caption light even when Windows uses dark mode.
    slint::Timer::single_shot(Duration::from_millis(60), light_caption);
    {
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(60), move || {
            if let Some(ui) = weak.upgrade() {
                watch_repaints(&ui);
            }
        });
    }
    if std::env::args().any(|arg| arg == "--preview-dirty") {
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(700), move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_inner_gap("12".into());
            }
        });
    }
    ui.on_open_github(|| {
        crate::ui::tray::open("https://github.com/ponslookspain/tile-rs", "");
    });
    ui.on_open_support(|| {
        crate::ui::tray::open(crate::ui::tray::SUPPORT_URL, "");
    });
    ui.on_open_config(|| {
        config::ensure_default();
        crate::ui::tray::open(
            "notepad.exe",
            &format!("\"{}\"", config::config_path().display()),
        );
    });
    ui.on_open_log(|| {
        crate::ui::tray::open(
            "notepad.exe",
            &format!("\"{}\"", crate::diagnostics::log_file().display()),
        );
    });
    ui.on_quit_manager(|| {
        crate::ui::tray::request_manager_quit();
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
        crate::diagnostics::log(format!("launcher event loop failed: {err}"));
    }
    // the tour stops watching when the window closes; it shows again next time until dismissed
    end_tour();
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

thread_local! {
    /// The settings window, for `force_full_repaint`.
    static REPAINT_TARGET: RefCell<Option<slint::Weak<MainWindow>>> = const { RefCell::new(None) };
}

/// Repaint the whole settings window once.
fn force_full_repaint() {
    REPAINT_TARGET.with(|target| {
        if let Some(ui) = target.borrow().as_ref().and_then(|weak| weak.upgrade()) {
            ui.set_full_repaint(!ui.get_full_repaint());
        }
    });
}

/// The software renderer only repaints what changed in the UI. When Windows itself drops the
/// window's pixels (restored after minimizing, shown again, resized, a DPI, display or theme
/// change), the rest would keep whatever was on screen before, often another window. These
/// messages ask for one full repaint, after the message has been handled.
unsafe extern "system" fn repaint_subclass(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{
        SWP_SHOWWINDOW, WINDOWPOS, WM_ACTIVATE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_SIZE,
        WM_THEMECHANGED, WM_WINDOWPOSCHANGED,
    };
    const WM_DWMCOMPOSITIONCHANGED: u32 = 0x031E;
    let shown = msg == WM_WINDOWPOSCHANGED
        && lparam.0 != 0
        && (*(lparam.0 as *const WINDOWPOS)).flags.contains(SWP_SHOWWINDOW);
    if shown
        || matches!(
            msg,
            WM_SIZE
                | WM_ACTIVATE
                | WM_DPICHANGED
                | WM_DISPLAYCHANGE
                | WM_THEMECHANGED
                | WM_DWMCOMPOSITIONCHANGED
        )
    {
        let _ = slint::invoke_from_event_loop(force_full_repaint);
    }
    windows::Win32::UI::Shell::DefSubclassProc(hwnd, msg, wparam, lparam)
}

fn watch_repaints(ui: &MainWindow) {
    REPAINT_TARGET.with(|target| *target.borrow_mut() = Some(ui.as_weak()));
    let Ok(hwnd) = (unsafe { FindWindowW(PCWSTR::null(), LAUNCHER_TITLE) }) else {
        return;
    };
    unsafe {
        let _ = windows::Win32::UI::Shell::SetWindowSubclass(hwnd, Some(repaint_subclass), 1, 0);
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
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn populate(ui: &MainWindow, cfg: &config::Config) {
    ui.set_layout_index(launcher_model::layout_index(cfg.layout));
    ui.set_indicator_size(launcher_model::indicator_index(
        cfg.workspace_indicator_size,
    ));
    ui.set_animations(cfg.animations);
    ui.set_auto_overflow(cfg.auto_overflow);
    ui.set_show_indicator(cfg.workspace_indicator);
    ui.set_focus_follows_mouse(cfg.focus_follows_mouse);
    ui.set_highlight_window(cfg.border);
    ui.set_autostart(crate::platform::autostart::autostart_enabled());
    ui.set_inner_gap(cfg.inner_gap.to_string().into());
    ui.set_outer_gap(cfg.outer_gap.to_string().into());
    ui.set_animation_ms(cfg.animation_ms.to_string().into());
    ui.set_easing(launcher_model::easing_name(cfg.easing).into());
    ui.set_resize_step(cfg.resize_step.to_string().into());
    ui.set_min_tile_width(cfg.min_tile_width.to_string().into());
    ui.set_min_tile_height(cfg.min_tile_height.to_string().into());
    ui.set_max_tiles(cfg.max_tiles.to_string().into());
    ui.set_accent_color(launcher_model::color_hex(cfg.accent_color).into());
    ui.set_border_color(launcher_model::color_hex(cfg.border_color).into());
    ui.set_border_width(cfg.border_width.to_string().into());
    ui.set_fullscreen_windows(launcher_model::fullscreen_index(cfg.fullscreen_windows));
    ui.set_media_windows(launcher_model::media_index(cfg.media_windows));
    ui.set_smart_gaps(cfg.smart_gaps);
    ui.set_master_count(cfg.master_count.to_string().into());
    ui.set_status("".into()); // empty: the header shows the page subtitle
}

/// UI state stays on Slint's thread; only network and installer work run on workers.
fn wire_updates(ui: &MainWindow) {
    let release = Arc::new(Mutex::new(None::<crate::platform::update::Release>));
    ui.on_check_updates({
        let weak = ui.as_weak();
        let release = release.clone();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if matches!(ui.get_update_state(), 1 | 4) {
                return;
            }
            ui.set_update_state(1);
            ui.set_update_message("Looking up the latest release…".into());
            let weak = ui.as_weak();
            let release = release.clone();
            if let Err(err) = thread::Builder::new()
                .name("launcher-update-check".into())
                .spawn(move || {
                    let result = crate::platform::update::check_latest();
                    let _ = weak.upgrade_in_event_loop(move |ui| match result {
                        Ok(Some(found)) => {
                            ui.set_update_state(2);
                            ui.set_update_version(found.version.clone().into());
                            ui.set_release_notes(found.notes.clone().into());
                            ui.set_update_message(
                                "The installer opens after the download, then tile-rs restarts."
                                    .into(),
                            );
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
                            ui.set_update_message(
                                format!("The check failed: {err}. Press Check to try again.")
                                    .into(),
                            );
                        }
                    });
                })
            {
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
            let worker_ui = weak;
            if let Err(err) = thread::Builder::new().name("launcher-update-install".into()).spawn(move || {
                let progress_ui = worker_ui.clone();
                let result = crate::platform::update::install(&found, move |percent| {
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
        move || {
            let url = release
                .lock()
                .unwrap()
                .as_ref()
                .map(|r| r.url.clone())
                .unwrap_or_else(|| {
                    "https://github.com/ponslookspain/tile-rs/releases/latest".into()
                });
            crate::ui::tray::open(&url, "");
        }
    });
    // Populate the update badge as soon as the launcher opens without blocking its event loop.
    ui.invoke_check_updates();
}

/// Adapt the toolkit-independent RGB value to a Slint swatch.
fn parse_hex_color(text: &str) -> slint::Color {
    match launcher_model::parse_hex_color(text) {
        Some([r, g, b]) => slint::Color::from_rgb_u8(r, g, b),
        None => slint::Color::from_argb_u8(0, 0, 0, 0),
    }
}

/// Where the "welcome" card remembers that it was shown.
fn welcome_marker() -> std::path::PathBuf {
    config::config_path().with_file_name("welcome-shown")
}

/// While the first-run tour is open, the manager notes the user's first moves in the progress
/// file; it only does so while the marker file exists.
fn start_tour() {
    // emptied rather than deleted: the manager may hold it open for a moment while appending
    let _ = std::fs::write(crate::app::tour_progress_path(), "");
    let _ = std::fs::write(crate::app::tour_marker(), "1");
}

fn end_tour() {
    let _ = std::fs::remove_file(crate::app::tour_marker());
    let _ = std::fs::write(crate::app::tour_progress_path(), "");
}

fn push_tour(ui: &MainWindow, editor: &Editor) {
    let done = launcher_model::tour_progress(
        &std::fs::read_to_string(crate::app::tour_progress_path()).unwrap_or_default(),
    );
    let steps: Vec<TourStep> = launcher_model::tour_texts(&editor.rows)
        .into_iter()
        .zip(done)
        .map(|(text, done)| TourStep {
            text: text.into(),
            done,
        })
        .collect();
    ui.set_tour_steps(ModelRc::new(VecModel::from(steps)));
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
    /// Autosave ticks left to wait for the manager's answer about refused keys, and the file's
    /// time stamp before the reload was requested (a new stamp means the answer is there).
    recheck: u8,
    refused_before: Option<std::time::SystemTime>,
    /// The form as last saved, and as seen on the previous autosave tick: a save happens once the
    /// form differs from the first and has stopped changing (the user paused typing).
    saved_form: String,
    last_form: String,
    /// Hotkeys or rules were edited since the last save (they are not part of `form_values`).
    edited: bool,
    /// Autosave ticks so far (for slow periodic work).
    ticks: u32,
}

fn read_refused() -> Vec<(String, String)> {
    launcher_model::parse_refused(
        &std::fs::read_to_string(config::hotkey_failures_path()).unwrap_or_default(),
    )
}

/// When the manager last wrote the refused-hotkeys file (it rewrites it on every reload).
fn refused_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata(config::hotkey_failures_path())
        .and_then(|m| m.modified())
        .ok()
}

/// Every form value in one string: autosave compares it with the saved and the previous one.
fn form_values(ui: &MainWindow) -> String {
    [
        ui.get_layout_index().to_string(),
        ui.get_indicator_size().to_string(),
        ui.get_animations().to_string(),
        ui.get_auto_overflow().to_string(),
        ui.get_show_indicator().to_string(),
        ui.get_focus_follows_mouse().to_string(),
        ui.get_highlight_window().to_string(),
        ui.get_autostart().to_string(),
        ui.get_inner_gap().to_string(),
        ui.get_outer_gap().to_string(),
        ui.get_animation_ms().to_string(),
        ui.get_easing().to_string(),
        ui.get_resize_step().to_string(),
        ui.get_min_tile_width().to_string(),
        ui.get_min_tile_height().to_string(),
        ui.get_max_tiles().to_string(),
        ui.get_border_color().to_string(),
        ui.get_accent_color().to_string(),
        ui.get_border_width().to_string(),
        ui.get_fullscreen_windows().to_string(),
        ui.get_media_windows().to_string(),
        ui.get_smart_gaps().to_string(),
        ui.get_master_count().to_string(),
    ]
    .join("\u{1f}")
}

fn has_changes(ui: &MainWindow, editor: &Editor) -> bool {
    editor.edited || form_values(ui) != editor.saved_form
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
            refused: launcher_model::is_refused(row, &editor.refused),
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
        apps: crate::platform::window::running_apps(),
        // the file describes the running manager's hotkeys; without a manager it is stale
        refused: if crate::ui::tray::manager_running() {
            read_refused()
        } else {
            Vec::new()
        },
        recheck: 0,
        refused_before: None,
        saved_form: form_values(ui),
        last_form: form_values(ui),
        edited: false,
        ticks: 0,
    }));
    {
        let ed = editor.borrow();
        push_rows(ui, &ed);
        push_rules(ui, &ed);
        push_apps(ui, &ed);
    }
    let welcome = !welcome_marker().exists();
    ui.set_show_welcome(welcome);
    if welcome {
        start_tour();
        push_tour(ui, &editor.borrow());
    } else if crate::app::tour_marker().exists() {
        end_tour(); // left behind by a launcher that did not exit cleanly
    }
    ui.on_dismiss_welcome({
        let weak = ui.as_weak();
        move || {
            let _ = std::fs::write(welcome_marker(), "1");
            end_tour();
            if let Some(ui) = weak.upgrade() {
                ui.set_show_welcome(false);
            }
        }
    });
    ui.on_resume_tiling({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if crate::ui::tray::request_manager_resume() {
                ui.set_paused(false);
                ui.set_status("Tiling resumed".into());
            }
        }
    });
    ui.on_reset_all({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ask_yes_no(
                "Reset every setting, hotkey and app rule to the defaults?\n\n\
                 A copy of your current config file is kept next to it.",
            ) {
                return;
            }
            reset_all(&ui, &mut editor.borrow_mut());
        }
    });
    ui.on_pause_tiling({
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if crate::ui::tray::request_manager_pause() {
                ui.invoke_dismiss_welcome();
                ui.set_status("Tiling is paused. Resume it from the tray icon menu.".into());
            } else {
                ui.set_status("tile-rs is not running, so there is nothing to pause".into());
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
            let text =
                launcher_model::resolve_text(&text, &held_keys(), alt || ctrl || shift || meta);
            match launcher_model::capture_key(&text, alt, ctrl, shift, meta) {
                Capture::Wait => {}
                Capture::Cancel => ui.set_recording(-1),
                Capture::Clear => {
                    row.keys.clear();
                    ui.set_recording(-1);
                    push_rows(&ui, &ed);
                    ed.edited = true;
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
                    ed.edited = true;
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
            ed.edited = true;
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
            ed.edited = true;
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
            ed.apps = crate::platform::window::running_apps();
            push_apps(&ui, &ed);
        }
    });
    ui.on_apply_preset({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move |preset| {
            let Some(ui) = weak.upgrade() else { return };
            let mut ed = editor.borrow_mut();
            ed.rows = launcher_model::preset_rows(preset);
            ui.set_recording(-1);
            push_rows(&ui, &ed);
            ed.edited = true;
            ui.set_dirty(true);
            ui.set_status(
                if preset == 1 {
                    "Hotkeys now use Win + Alt (custom keys were replaced)"
                } else {
                    "Hotkeys now use Alt (custom keys were replaced)"
                }
                .into(),
            );
        }
    });
    ui.on_add_rule_from_file({
        let weak = ui.as_weak();
        let editor = editor.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            let Some(process) = pick_program() else {
                return;
            };
            add_rule(&ui, &mut editor.borrow_mut(), process);
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
            add_rule(&ui, &mut ed, process);
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
                    ed.edited = true;
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
            let Some(ui) = weak.upgrade() else {
                return HideWindow;
            };
            let mut ed = editor.borrow_mut();
            if has_changes(&ui, &ed) && ui.get_recording() < 0 {
                save(&ui, &mut ed);
            }
            if !has_changes(&ui, &ed) {
                return HideWindow;
            }
            // the save failed (or a key is being recorded): say why and let the user choose
            if ask_close_anyway(&ui.get_status()) == IDYES {
                HideWindow
            } else {
                KeepWindowShown
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
            if ui.get_show_welcome() {
                push_tour(&ui, &ed);
                // keep the marker fresh: the manager ignores one older than an hour
                if ed.ticks.is_multiple_of(150) {
                    let _ = std::fs::write(crate::app::tour_marker(), "1");
                }
            }
            // every 2 s is enough for a banner; no answer keeps the last known state
            if ed.ticks.is_multiple_of(5) {
                if let Some(paused) = crate::ui::tray::manager_paused() {
                    ui.set_paused(paused);
                }
            }
            ed.ticks += 1;
            if ed.recheck > 0 {
                ed.recheck -= 1;
                // the manager rewrote the file: its answer is in (no answer within ~3 s: keep the
                // previous marks rather than show stale ones as fresh)
                if refused_stamp() != ed.refused_before {
                    ed.recheck = 0;
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
            let form = form_values(&ui);
            let changed = ed.edited || form != ed.saved_form;
            ui.set_dirty(changed);
            let settled = form == ed.last_form;
            ed.last_form = form;
            if changed && settled && ui.get_recording() < 0 {
                save(&ui, &mut ed);
            }
        },
    );
    timer
}

/// Adds a rule for `process` (an executable name without `.exe`) with the chosen action.
fn add_rule(ui: &MainWindow, ed: &mut Editor, process: String) {
    if !launcher_model::add_process_rule(&mut ed.rules, process, ui.get_rule_kind_index()) {
        ui.set_status("This rule already exists".into());
        return;
    }
    ed.rules_dirty = true;
    push_rules(ui, ed);
    ed.edited = true;
    ui.set_dirty(true);
    ui.set_status("Rule added".into());
}

/// Lets the user choose a program file; returns its name without `.exe`.
fn pick_program() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut file = vec![0u16; 1024];
    let filter: Vec<u16> = "Programs (*.exe)\0*.exe\0\0".encode_utf16().collect();
    let title = crate::ui::tray::wide("Choose a program");
    let owner = unsafe { FindWindowW(PCWSTR::null(), LAUNCHER_TITLE) }.unwrap_or_default();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(file.as_mut_ptr()),
        nMaxFile: file.len() as u32,
        lpstrTitle: PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if !unsafe { GetOpenFileNameW(&mut dialog) }.as_bool() {
        return None;
    }
    let end = file.iter().position(|c| *c == 0).unwrap_or(file.len());
    let path = String::from_utf16_lossy(&file[..end]);
    std::path::Path::new(&path)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
}

/// The letter, digit and punctuation keys physically held right now, independent of the keyboard
/// layout and of Shift; arrows, F-keys and the like are reported reliably by Slint itself.
fn held_keys() -> Vec<char> {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    (0x30u16..=0xde)
        .filter(|vk| unsafe { GetAsyncKeyState(i32::from(*vk)) } < 0)
        .filter_map(launcher_model::vk_char)
        .collect()
}

fn ask_yes_no(question: &str) -> bool {
    let owner = unsafe { FindWindowW(PCWSTR::null(), LAUNCHER_TITLE) }.ok();
    let text = crate::ui::tray::wide(question);
    let answer = unsafe {
        MessageBoxW(
            owner,
            PCWSTR(text.as_ptr()),
            LAUNCHER_TITLE,
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        )
    };
    answer == IDYES
}

/// Writes the built-in default config (after keeping a copy of the current one), shows it in the
/// window and applies it.
fn reset_all(ui: &MainWindow, editor: &mut Editor) {
    let path = config::config_path();
    let text = config::default_config();
    let cfg = match config::parse(&text) {
        Ok(cfg) => cfg,
        Err(err) => {
            ui.set_status(format!("Could not read the defaults: {err}").into());
            return;
        }
    };
    // every reset keeps its own copy, so a second reset cannot overwrite the user's real config
    let current = std::fs::read_to_string(&path).ok();
    if current.as_deref().is_some_and(|c| c != text) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let backup = path.with_file_name(format!("config.toml.before-reset-{stamp}"));
        if let Err(err) = std::fs::copy(&path, &backup) {
            ui.set_status(format!("Could not keep a copy of the config: {err}").into());
            return;
        }
    }
    if let Err(err) = config::save_atomic(&path, &text) {
        ui.set_status(format!("Could not reset the settings: {err}").into());
        return;
    }
    config::set(cfg.clone());
    let autostart = ui.get_autostart(); // not part of the config file; left as it is
    populate(ui, &cfg);
    ui.set_autostart(autostart);
    editor.rows = launcher_model::rows_from_bindings(&cfg.bindings);
    editor.rules = cfg.rules;
    editor.rules_dirty = false;
    editor.edited = false;
    ui.set_recording(-1);
    push_rows(ui, editor);
    push_rules(ui, editor);
    editor.saved_form = form_values(ui);
    editor.last_form = editor.saved_form.clone();
    ui.set_dirty(false);
    editor.refused_before = refused_stamp();
    if crate::ui::tray::request_manager_reload() {
        editor.recheck = 8;
    }
    ui.set_status("All settings are back to the defaults".into());
}

fn ask_close_anyway(reason: &str) -> MESSAGEBOX_RESULT {
    let owner = unsafe { FindWindowW(PCWSTR::null(), LAUNCHER_TITLE) }.ok();
    let text = crate::ui::tray::wide(&format!(
        "Some changes are not saved yet:\n{reason}\n\nClose anyway and lose them?"
    ));
    unsafe {
        MessageBoxW(
            owner,
            PCWSTR(text.as_ptr()),
            LAUNCHER_TITLE,
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        )
    }
}

fn save(ui: &MainWindow, editor: &mut Editor) {
    let form = launcher_model::SettingsForm {
        layout_index: ui.get_layout_index(),
        indicator_size: ui.get_indicator_size(),
        inner_gap: ui.get_inner_gap().to_string(),
        outer_gap: ui.get_outer_gap().to_string(),
        animations: ui.get_animations(),
        animation_ms: ui.get_animation_ms().to_string(),
        easing: ui.get_easing().to_string(),
        resize_step: ui.get_resize_step().to_string(),
        min_tile_width: ui.get_min_tile_width().to_string(),
        min_tile_height: ui.get_min_tile_height().to_string(),
        max_tiles: ui.get_max_tiles().to_string(),
        auto_overflow: ui.get_auto_overflow(),
        smart_gaps: ui.get_smart_gaps(),
        master_count: ui.get_master_count().to_string(),
        show_indicator: ui.get_show_indicator(),
        focus_follows_mouse: ui.get_focus_follows_mouse(),
        highlight_window: ui.get_highlight_window(),
        border_color: ui.get_border_color().to_string(),
        accent_color: ui.get_accent_color().to_string(),
        border_width: ui.get_border_width().to_string(),
        fullscreen_windows: ui.get_fullscreen_windows(),
        media_windows: ui.get_media_windows(),
    };
    let pairs = match form.config_pairs() {
        Ok(pairs) => pairs,
        Err(message) => {
            ui.set_status(message.into());
            return;
        }
    };
    if launcher_model::conflicts(&editor.rows).iter().any(|c| *c) {
        ui.set_status("Fix the keys marked ⚠: one combination cannot be used twice".into());
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
    let mut updated = launcher_model::edit_hotkeys(
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
    editor.edited = false;
    editor.saved_form = form_values(ui);
    ui.set_dirty(false);
    editor.refused_before = refused_stamp();
    if !crate::platform::autostart::set_autostart(ui.get_autostart()) {
        ui.set_status("Saved, but starting with Windows could not be changed".into());
    } else if crate::ui::tray::request_manager_reload() {
        editor.recheck = 8; // wait up to ~3 s for the manager's answer about refused keys
        ui.set_status("Settings applied".into());
    } else {
        ui.set_status("Saved; tile-rs is not running, so they apply at its next start".into());
    }
}
