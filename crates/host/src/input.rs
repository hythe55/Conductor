//! Keyboard and mouse injection with SendInput, and pens through [`crate::pen`].
//!
//! Keys arrive as DOM `KeyboardEvent.code` values (physical positions) and are
//! sent as scan codes, so the host's own keyboard layout decides the character.

use std::collections::HashSet;

use conductor_proto::MouseButton;
use conductor_proto::keys::{TYPED, scancode};
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
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: TYPED },
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
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: TYPED },
        },
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
                dwExtraInfo: TYPED,
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
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(0),
                            wScan: unit,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: TYPED,
                        },
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
