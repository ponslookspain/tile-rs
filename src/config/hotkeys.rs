//! Parse keyboard combinations into Windows virtual keys and modifiers.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN,
};

/// "alt+shift+h" -> (modifiers, virtual-key code)
pub fn parse_hotkey(spec: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
    let mut mods = HOT_KEY_MODIFIERS(0);
    let mut key: Option<u32> = None;
    for part in spec.split('+') {
        let p = part.trim().to_ascii_lowercase();
        match p.as_str() {
            "alt" => mods |= MOD_ALT,
            "ctrl" | "control" => mods |= MOD_CONTROL,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" => mods |= MOD_WIN,
            "" => return Err(format!("empty part in \"{spec}\"")),
            other => {
                if key.is_some() {
                    return Err(format!("\"{spec}\": more than one key"));
                }
                key = Some(
                    parse_key(other)
                        .ok_or_else(|| format!("unknown key \"{other}\" in \"{spec}\""))?,
                );
            }
        }
    }
    match key {
        Some(k) => Ok((mods, k)),
        None => Err(format!("\"{spec}\": no key")),
    }
}

fn parse_key(k: &str) -> Option<u32> {
    let b = k.as_bytes();
    if b.len() == 1 {
        let c = b[0];
        if c.is_ascii_lowercase() {
            return Some(c.to_ascii_uppercase() as u32);
        }
        if c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=12).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    Some(match k {
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "numpad_add" => 0x6B,
        "numpad_subtract" => 0x6D,
        "minus" => 0xBD,
        "plus" | "equal" => 0xBB,
        "comma" => 0xBC,
        "period" => 0xBE,
        "semicolon" => 0xBA,
        "slash" => 0xBF,
        "grave" => 0xC0,
        "bracketleft" => 0xDB,
        "backslash" => 0xDC,
        "bracketright" => 0xDD,
        "quote" => 0xDE,
        _ => return None,
    })
}
