//! Keyboard and mouse injection with SendInput, and pens through [`crate::pen`].
//!
//! Keys arrive as DOM `KeyboardEvent.code` values (physical positions) and are
//! sent as scan codes, so the host's own keyboard layout decides the character.

use std::collections::HashSet;

use conductor_proto::MouseButton;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub use crate::pen::Pen;
use crate::pen::PenInjector;

pub struct Injector {
    /// Streamed display rectangle in virtual-desktop pixels.
    display: (i32, i32, u32, u32),
    held_keys: HashSet<String>,
    held_buttons: HashSet<u8>,
    pen: PenInjector,
    /// The pen is touching, on a PC where it works as a mouse.
    pen_mouse_down: bool,
}

fn send(inputs: &[INPUT]) {
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    // Fails while a secure desktop (lock screen, UAC) has input; move there and retry.
    if (sent as usize) < inputs.len() && crate::desktop::follow_input() {
        unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key(scan: u16, extended: bool, up: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn vk(code: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: code,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn button_id(b: MouseButton) -> u8 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
    }
}

impl Injector {
    pub fn new() -> Self {
        Self {
            display: (0, 0, 1, 1),
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            pen: PenInjector::default(),
            pen_mouse_down: false,
        }
    }

    pub fn set_display(&mut self, origin: (i32, i32), size: (u32, u32)) {
        self.display = (origin.0, origin.1, size.0.max(1), size.1.max(1));
    }

    /// A point normalized to the streamed display, in virtual-desktop pixels.
    fn pixel(&self, x: f32, y: f32) -> (f64, f64) {
        let (dx, dy, dw, dh) = self.display;
        (dx as f64 + (x.clamp(0.0, 1.0) as f64) * (dw - 1) as f64, dy as f64 + (y.clamp(0.0, 1.0) as f64) * (dh - 1) as f64)
    }

    pub fn mouse_move(&self, x: f32, y: f32) {
        let (px, py) = self.pixel(x, y);
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
                GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
            )
        };
        let nx = ((px - vx as f64) * 65535.0 / (vw - 1) as f64).round() as i32;
        let ny = ((py - vy as f64) * 65535.0 / (vh - 1) as f64).round() as i32;
        send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0)]);
    }

    pub fn mouse_delta(&self, dx: i32, dy: i32) {
        send(&[mouse(MOUSEEVENTF_MOVE, dx, dy, 0)]);
    }

    pub fn button(&mut self, b: MouseButton, down: bool) {
        let id = button_id(b);
        if down {
            self.held_buttons.insert(id);
        } else {
            self.held_buttons.remove(&id);
        }
        send(&[button_input(b, down)]);
    }

    pub fn wheel(&self, dx: i32, dy: i32) {
        let mut inputs = Vec::new();
        if dy != 0 {
            inputs.push(mouse(MOUSEEVENTF_WHEEL, 0, 0, -dy));
        }
        if dx != 0 {
            inputs.push(mouse(MOUSEEVENTF_HWHEEL, 0, 0, dx));
        }
        send(&inputs);
    }

    pub fn pen(&mut self, p: Pen) {
        let (px, py) = self.pixel(p.x, p.y);
        if self.pen.update(&p, (px.round() as i32, py.round() as i32)) {
            return;
        }
        self.mouse_move(p.x, p.y);
        if p.contact != self.pen_mouse_down {
            self.pen_mouse_down = p.contact;
            self.button(MouseButton::Left, p.contact);
        }
    }

    pub fn pen_leave(&mut self) {
        self.pen.leave();
        if self.pen_mouse_down {
            self.pen_mouse_down = false;
            self.button(MouseButton::Left, false);
        }
    }

    /// Called every so often while the session runs.
    pub fn tick(&mut self) {
        self.pen.tick();
    }

    pub fn key(&mut self, code: &str, down: bool) {
        if down {
            self.held_keys.insert(code.to_string());
        } else {
            self.held_keys.remove(code);
        }
        // Pause has a scan code sequence SendInput can't express; use its virtual key.
        if code == "Pause" {
            send(&[vk(VK_PAUSE, !down)]);
            return;
        }
        if let Some((scan, ext)) = scancode(code) {
            send(&[key(scan, ext, !down)]);
        }
    }

    /// Presses Ctrl+V, or only V while Ctrl is held already.
    pub fn paste(&mut self) {
        let ctrl = self.held_keys.contains("ControlLeft") || self.held_keys.contains("ControlRight");
        if !ctrl {
            self.key("ControlLeft", true);
        }
        self.key("KeyV", true);
        self.key("KeyV", false);
        if !ctrl {
            self.key("ControlLeft", false);
        }
    }

    pub fn text(&self, text: &str) {
        let mut inputs = Vec::new();
        for unit in text.encode_utf16() {
            for up in [false, true] {
                let mut flags = KEYEVENTF_UNICODE;
                if up {
                    flags |= KEYEVENTF_KEYUP;
                }
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: unit, dwFlags: flags, time: 0, dwExtraInfo: 0 },
                    },
                });
            }
        }
        send(&inputs);
    }

    pub fn release_all(&mut self) {
        self.pen_leave();
        for code in std::mem::take(&mut self.held_keys) {
            self.key(&code, false);
        }
        for id in std::mem::take(&mut self.held_buttons) {
            let b = [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward]
                [id as usize];
            send(&[button_input(b, false)]);
        }
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_all();
    }
}

fn button_input(b: MouseButton, down: bool) -> INPUT {
    let (flags, data) = match (b, down) {
        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, 1),
        (MouseButton::Back, false) => (MOUSEEVENTF_XUP, 1),
        (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
        (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, 2),
    };
    mouse(flags, 0, 0, data)
}

/// Set 1 scan code for a DOM `KeyboardEvent.code`, and whether it is an E0 (extended) key.
pub fn scancode(code: &str) -> Option<(u16, bool)> {
    let plain = |s: u16| Some((s, false));
    let ext = |s: u16| Some((s, true));
    if let Some(c) = code.strip_prefix("Key") {
        let c = c.as_bytes();
        if c.len() == 1 && c[0].is_ascii_uppercase() {
            const LETTERS: [u16; 26] = [
                0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19,
                0x10, 0x13, 0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
            ];
            return plain(LETTERS[(c[0] - b'A') as usize]);
        }
    }
    if let Some(d) = code.strip_prefix("Digit") {
        if let Ok(n) = d.parse::<u16>() {
            if n <= 9 {
                return plain(if n == 0 { 0x0B } else { 0x01 + n });
            }
        }
    }
    if let Some(f) = code.strip_prefix('F') {
        if let Ok(n) = f.parse::<u16>() {
            return match n {
                1..=10 => plain(0x3A + n),
                11 => plain(0x57),
                12 => plain(0x58),
                13..=23 => plain(0x64 + (n - 13)),
                24 => plain(0x76),
                _ => None,
            };
        }
    }
    match code {
        "Escape" => plain(0x01),
        "Minus" => plain(0x0C),
        "Equal" => plain(0x0D),
        "Backspace" => plain(0x0E),
        "Tab" => plain(0x0F),
        "BracketLeft" => plain(0x1A),
        "BracketRight" => plain(0x1B),
        "Enter" => plain(0x1C),
        "ControlLeft" => plain(0x1D),
        "Semicolon" => plain(0x27),
        "Quote" => plain(0x28),
        "Backquote" => plain(0x29),
        "ShiftLeft" => plain(0x2A),
        "Backslash" => plain(0x2B),
        "Comma" => plain(0x33),
        "Period" => plain(0x34),
        "Slash" => plain(0x35),
        "ShiftRight" => plain(0x36),
        "NumpadMultiply" => plain(0x37),
        "AltLeft" => plain(0x38),
        "Space" => plain(0x39),
        "CapsLock" => plain(0x3A),
        "NumLock" => plain(0x45),
        "ScrollLock" => plain(0x46),
        "Numpad7" => plain(0x47),
        "Numpad8" => plain(0x48),
        "Numpad9" => plain(0x49),
        "NumpadSubtract" => plain(0x4A),
        "Numpad4" => plain(0x4B),
        "Numpad5" => plain(0x4C),
        "Numpad6" => plain(0x4D),
        "NumpadAdd" => plain(0x4E),
        "Numpad1" => plain(0x4F),
        "Numpad2" => plain(0x50),
        "Numpad3" => plain(0x51),
        "Numpad0" => plain(0x52),
        "NumpadDecimal" => plain(0x53),
        "IntlBackslash" => plain(0x56),
        "IntlRo" => plain(0x73),
        "IntlYen" => plain(0x7D),
        "NumpadEnter" => ext(0x1C),
        "ControlRight" => ext(0x1D),
        "NumpadDivide" => ext(0x35),
        "PrintScreen" => ext(0x37),
        "AltRight" => ext(0x38),
        "Home" => ext(0x47),
        "ArrowUp" => ext(0x48),
        "PageUp" => ext(0x49),
        "ArrowLeft" => ext(0x4B),
        "ArrowRight" => ext(0x4D),
        "End" => ext(0x4F),
        "ArrowDown" => ext(0x50),
        "PageDown" => ext(0x51),
        "Insert" => ext(0x52),
        "Delete" => ext(0x53),
        "MetaLeft" | "OSLeft" => ext(0x5B),
        "MetaRight" | "OSRight" => ext(0x5C),
        "ContextMenu" => ext(0x5D),
        "AudioVolumeMute" => ext(0x20),
        "AudioVolumeDown" => ext(0x2E),
        "AudioVolumeUp" => ext(0x30),
        "MediaPlayPause" => ext(0x22),
        "MediaStop" => ext(0x24),
        "MediaTrackNext" => ext(0x19),
        "MediaTrackPrevious" => ext(0x10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::scancode;

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
        assert_eq!(scancode("Unidentified"), None);
    }
}
