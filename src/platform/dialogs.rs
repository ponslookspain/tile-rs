//! Native notifications that leave the manager message loop responsive.
use crate::ui::tray;
use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_TOPMOST, MB_YESNO,
};

/// Message box. Returns true if the user pressed Yes (only meaningful when `question`).
pub fn msgbox(title: &str, text: &str, question: bool) -> bool {
    let (t, x) = (tray::wide(title), tray::wide(text));
    let flags = if question {
        MB_YESNO | MB_ICONQUESTION
    } else {
        MB_OK | MB_ICONINFORMATION
    };
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(x.as_ptr()),
            PCWSTR(t.as_ptr()),
            flags | MB_TOPMOST,
        ) == IDYES
    }
}

/// Non-blocking message box (own thread), so the main message loop keeps running.
pub fn notify(title: &str, text: &str) {
    let (t, x) = (title.to_string(), text.to_string());
    std::thread::spawn(move || {
        msgbox(&t, &x, false);
    });
}
