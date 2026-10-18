//! Keys by their DOM `KeyboardEvent.code`, which names where a key is rather
//! than what it types, and their PC (set 1) scan codes.

/// Marks the keys and clicks the host types, so the app on the same PC
/// leaves them alone rather than sending them back.
pub const TYPED: usize = 0x434E_4454;

/// Scan codes of the letter keys, A to Z.
const LETTERS: [u16; 26] = [
    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13, 0x1F,
    0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
];

/// The other keys: code, scan code, and whether it's an E0 (extended) key.
const NAMED: &[(&str, u16, bool)] = &[
    ("Escape", 0x01, false),
    ("Minus", 0x0C, false),
    ("Equal", 0x0D, false),
    ("Backspace", 0x0E, false),
    ("Tab", 0x0F, false),
    ("BracketLeft", 0x1A, false),
    ("BracketRight", 0x1B, false),
    ("Enter", 0x1C, false),
    ("ControlLeft", 0x1D, false),
    ("Semicolon", 0x27, false),
    ("Quote", 0x28, false),
    ("Backquote", 0x29, false),
    ("ShiftLeft", 0x2A, false),
    ("Backslash", 0x2B, false),
    ("Comma", 0x33, false),
    ("Period", 0x34, false),
    ("Slash", 0x35, false),
    ("ShiftRight", 0x36, false),
    ("NumpadMultiply", 0x37, false),
    ("AltLeft", 0x38, false),
    ("Space", 0x39, false),
    ("CapsLock", 0x3A, false),
    ("NumLock", 0x45, false),
    ("ScrollLock", 0x46, false),
    ("Numpad7", 0x47, false),
    ("Numpad8", 0x48, false),
    ("Numpad9", 0x49, false),
    ("NumpadSubtract", 0x4A, false),
    ("Numpad4", 0x4B, false),
    ("Numpad5", 0x4C, false),
    ("Numpad6", 0x4D, false),
    ("NumpadAdd", 0x4E, false),
    ("Numpad1", 0x4F, false),
    ("Numpad2", 0x50, false),
    ("Numpad3", 0x51, false),
    ("Numpad0", 0x52, false),
    ("NumpadDecimal", 0x53, false),
    ("IntlBackslash", 0x56, false),
    ("IntlRo", 0x73, false),
    ("IntlYen", 0x7D, false),
    ("NumpadEnter", 0x1C, true),
    ("ControlRight", 0x1D, true),
    ("NumpadDivide", 0x35, true),
    ("PrintScreen", 0x37, true),
    ("AltRight", 0x38, true),
    ("Home", 0x47, true),
    ("ArrowUp", 0x48, true),
    ("PageUp", 0x49, true),
    ("ArrowLeft", 0x4B, true),
    ("ArrowRight", 0x4D, true),
    ("End", 0x4F, true),
    ("ArrowDown", 0x50, true),
    ("PageDown", 0x51, true),
    ("Insert", 0x52, true),
    ("Delete", 0x53, true),
    ("MetaLeft", 0x5B, true),
    ("MetaRight", 0x5C, true),
    ("ContextMenu", 0x5D, true),
    ("AudioVolumeMute", 0x20, true),
    ("AudioVolumeDown", 0x2E, true),
    ("AudioVolumeUp", 0x30, true),
    ("MediaPlayPause", 0x22, true),
    ("MediaStop", 0x24, true),
    ("MediaTrackNext", 0x19, true),
    ("MediaTrackPrevious", 0x10, true),
];

/// Scan code for a key's code, and whether it is an E0 (extended) key.
pub fn scancode(code: &str) -> Option<(u16, bool)> {
    if let Some(c) = code.strip_prefix("Key") {
        if let [c @ b'A'..=b'Z'] = c.as_bytes() {
            return Some((LETTERS[(c - b'A') as usize], false));
        }
    }
    if let Some(n) = code.strip_prefix("Digit").and_then(|d| d.parse::<u16>().ok()) {
        return match n {
            0 => Some((0x0B, false)),
            1..=9 => Some((0x01 + n, false)),
            _ => None,
        };
    }
    if let Some(n) = code.strip_prefix('F').and_then(|f| f.parse::<u16>().ok()) {
        return match n {
            1..=10 => Some((0x3A + n, false)),
            11 => Some((0x57, false)),
            12 => Some((0x58, false)),
            13..=23 => Some((0x64 + (n - 13), false)),
            24 => Some((0x76, false)),
            _ => None,
        };
    }
    let code = match code {
        "OSLeft" => "MetaLeft",
        "OSRight" => "MetaRight",
        c => c,
    };
    NAMED.iter().find(|(c, ..)| *c == code).map(|&(_, scan, ext)| (scan, ext))
}

/// The code of the key with this scan code, the other way from [`scancode`].
pub fn code(scan: u16, extended: bool) -> Option<String> {
    if let Some(&(c, ..)) = NAMED.iter().find(|&&(_, s, e)| s == scan && e == extended) {
        return Some(c.into());
    }
    if extended {
        return None;
    }
    if let Some(i) = LETTERS.iter().position(|&s| s == scan) {
        return Some(format!("Key{}", (b'A' + i as u8) as char));
    }
    match scan {
        0x02..=0x0A => Some(format!("Digit{}", scan - 0x01)),
        0x0B => Some("Digit0".into()),
        0x3B..=0x44 => Some(format!("F{}", scan - 0x3A)),
        0x57 => Some("F11".into()),
        0x58 => Some("F12".into()),
        0x64..=0x6E => Some(format!("F{}", scan - 0x64 + 13)),
        0x76 => Some("F24".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{code, scancode};

    #[test]
    fn scancodes() {
        assert_eq!(scancode("KeyA"), Some((0x1E, false)));
        assert_eq!(scancode("KeyZ"), Some((0x2C, false)));
        assert_eq!(scancode("KeyQ"), Some((0x10, false)));
        assert_eq!(scancode("KeyM"), Some((0x32, false)));
        assert_eq!(scancode("Digit1"), Some((0x02, false)));
        assert_eq!(scancode("Digit0"), Some((0x0B, false)));
        assert_eq!(scancode("F1"), Some((0x3B, false)));
        assert_eq!(scancode("F10"), Some((0x44, false)));
        assert_eq!(scancode("F12"), Some((0x58, false)));
        assert_eq!(scancode("F13"), Some((0x64, false)));
        assert_eq!(scancode("ArrowLeft"), Some((0x4B, true)));
        assert_eq!(scancode("MetaLeft"), Some((0x5B, true)));
        assert_eq!(scancode("OSLeft"), Some((0x5B, true)));
        assert_eq!(scancode("Keya"), None);
        assert_eq!(scancode("Unidentified"), None);
    }

    #[test]
    fn codes_round_trip() {
        let mut codes: Vec<String> = (b'A'..=b'Z').map(|c| format!("Key{}", c as char)).collect();
        codes.extend((0..=9).map(|n| format!("Digit{n}")));
        codes.extend((1..=24).map(|n| format!("F{n}")));
        codes.extend(super::NAMED.iter().map(|(c, ..)| c.to_string()));
        for c in codes {
            let (scan, ext) = scancode(&c).unwrap_or_else(|| panic!("{c}"));
            assert_eq!(code(scan, ext).as_deref(), Some(c.as_str()));
        }
        assert_eq!(code(0x5B, true).as_deref(), Some("MetaLeft"));
        assert_eq!(code(0x0F, false).as_deref(), Some("Tab"));
        assert_eq!(code(0x7F, false), None);
    }
}
