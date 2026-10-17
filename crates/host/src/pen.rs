//! Pen input with pressure and tilt, through a synthetic pen (Windows 10 1809
//! and later). Apps that read pens see a real one, and the rest get the mouse
//! Windows makes from any pen.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE, POINTER_FEEDBACK_DEFAULT,
    POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows::Win32::UI::Input::Pointer::{
    InjectSyntheticPointerInput, POINTER_BUTTON_CHANGE_TYPE, POINTER_CHANGE_FIRSTBUTTON_DOWN, POINTER_CHANGE_FIRSTBUTTON_UP,
    POINTER_CHANGE_NONE, POINTER_FLAG_DOWN, POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE,
    POINTER_FLAG_PRIMARY, POINTER_FLAG_SECONDBUTTON, POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_FLAGS, POINTER_INFO,
    POINTER_PEN_INFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_ROTATION, PEN_MASK_TILT_X,
    PEN_MASK_TILT_Y, PT_PEN,
};

/// A pen on the client, from `ClientMsg::Pen`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pen {
    pub x: f32,
    pub y: f32,
    pub contact: bool,
    pub pressure: f32,
    pub tilt_x: i32,
    pub tilt_y: i32,
    pub twist: u32,
    pub barrel: bool,
    pub eraser: bool,
}

/// Windows lets go of a synthetic pen that goes quiet, so a pen that's still
/// there is sent again once it's been quiet this long (checked every session tick).
const REPEAT: Duration = Duration::from_millis(40);

/// Sends that fail in a row before the pen works as a mouse instead.
const GIVE_UP: u32 = 10;

const IN_RANGE: u32 = POINTER_FLAG_INRANGE.0;
const IN_CONTACT: u32 = POINTER_FLAG_INCONTACT.0;

struct Device(HSYNTHETICPOINTERDEVICE);

// A handle to the pen, which one session uses at a time.
unsafe impl Send for Device {}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { DestroySyntheticPointerDevice(self.0) };
    }
}

/// What was last sent about the pen.
#[derive(Clone, Copy)]
struct State {
    at: (i32, i32),
    flags: u32,
    change: POINTER_BUTTON_CHANGE_TYPE,
    pen_flags: u32,
    pressure: u32,
    tilt: (i32, i32),
    rotation: u32,
    sent: Instant,
}

#[derive(Default)]
pub struct PenInjector {
    device: Option<Device>,
    /// This PC can't have a synthetic pen, so the pen works as a mouse.
    unavailable: bool,
    last: Option<State>,
    /// Sends in a row that failed.
    failures: u32,
}

impl PenInjector {
    /// Sends where the pen is now, `at` in virtual-desktop pixels. Returns
    /// false when this PC can't have a synthetic pen.
    pub fn update(&mut self, p: &Pen, at: (i32, i32)) -> bool {
        if !self.ready() {
            return false;
        }
        // Windows only takes which end of the pen is in use as it comes into
        // range, and browsers only tell once the eraser touches, so the pen
        // leaves and comes back the other way round.
        if let Some(last) = self.last {
            let inverted = last.pen_flags & PEN_FLAG_INVERTED != 0;
            if last.flags & IN_RANGE != 0 && last.flags & IN_CONTACT == 0 && inverted != p.eraser {
                self.leave();
            }
        }
        // Comes into range before touching.
        if p.contact && !self.last.is_some_and(|s| s.flags & IN_RANGE != 0) {
            self.update(&Pen { contact: false, ..*p }, at);
        }
        let touching = self.last.is_some_and(|s| s.flags & IN_CONTACT != 0);
        let (flags, change) = if p.contact {
            let mut flags = IN_RANGE | IN_CONTACT | POINTER_FLAG_FIRSTBUTTON.0 | POINTER_FLAG_PRIMARY.0;
            if p.barrel {
                flags |= POINTER_FLAG_SECONDBUTTON.0;
            }
            if touching {
                (flags | POINTER_FLAG_UPDATE.0, POINTER_CHANGE_NONE)
            } else {
                (flags | POINTER_FLAG_DOWN.0, POINTER_CHANGE_FIRSTBUTTON_DOWN)
            }
        } else if touching {
            // Lifted. Hovering picks up again with the next update.
            (POINTER_FLAG_UP.0 | POINTER_FLAG_PRIMARY.0, POINTER_CHANGE_FIRSTBUTTON_UP)
        } else {
            (IN_RANGE | POINTER_FLAG_UPDATE.0 | POINTER_FLAG_PRIMARY.0, POINTER_CHANGE_NONE)
        };
        let mut pen_flags = 0;
        if p.barrel {
            pen_flags |= PEN_FLAG_BARREL;
        }
        if p.eraser {
            pen_flags |= PEN_FLAG_INVERTED;
            if p.contact {
                pen_flags |= PEN_FLAG_ERASER;
            }
        }
        self.send(State {
            at,
            flags,
            change,
            pen_flags,
            pressure: if p.contact { (p.pressure.clamp(0.0, 1.0) * 1024.0).round() as u32 } else { 0 },
            tilt: (p.tilt_x.clamp(-90, 90), p.tilt_y.clamp(-90, 90)),
            rotation: p.twist % 360,
            sent: Instant::now(),
        })
    }

    /// The pen moved away, or the session stopped.
    pub fn leave(&mut self) {
        let Some(last) = self.last else { return };
        let flags = if last.flags & IN_CONTACT != 0 {
            POINTER_FLAG_UP.0 | POINTER_FLAG_PRIMARY.0
        } else if last.flags & IN_RANGE != 0 {
            POINTER_FLAG_UPDATE.0 | POINTER_FLAG_PRIMARY.0
        } else {
            return;
        };
        let change = if last.flags & IN_CONTACT != 0 { POINTER_CHANGE_FIRSTBUTTON_UP } else { POINTER_CHANGE_NONE };
        self.send(State { flags, change, pressure: 0, sent: Instant::now(), ..last });
    }

    /// Called often, to keep a pen that hasn't moved from being dropped.
    pub fn tick(&mut self) {
        let Some(last) = self.last else { return };
        if last.flags & IN_RANGE == 0 || last.sent.elapsed() < REPEAT {
            return;
        }
        let flags = (last.flags & !(POINTER_FLAG_DOWN.0 | POINTER_FLAG_UP.0)) | POINTER_FLAG_UPDATE.0;
        self.send(State { flags, change: POINTER_CHANGE_NONE, sent: Instant::now(), ..last });
    }

    fn ready(&mut self) -> bool {
        if self.device.is_none() && !self.unavailable {
            match unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_DEFAULT) } {
                Ok(device) => self.device = Some(Device(device)),
                Err(e) => {
                    tracing::warn!("this PC can't have a synthetic pen, so the pen works as a mouse: {e}");
                    self.unavailable = true;
                }
            }
        }
        self.device.is_some()
    }

    /// Returns false once the pen has been given up on.
    fn send(&mut self, state: State) -> bool {
        let Some(device) = &self.device else { return false };
        let info = [pointer_info(&state)];
        let mut sent = unsafe { InjectSyntheticPointerInput(device.0, &info) };
        // Fails while a secure desktop (lock screen, UAC) has input; move there and retry.
        if sent.is_err() && crate::desktop::follow_input() {
            sent = unsafe { InjectSyntheticPointerInput(device.0, &info) };
        }
        match sent {
            Ok(()) => self.failures = 0,
            Err(e) => {
                self.failures += 1;
                if self.failures == 1 {
                    tracing::warn!("sending pen input: {e}");
                }
                if self.failures >= GIVE_UP {
                    tracing::warn!("pen input keeps failing, so the pen works as a mouse from now on");
                    // Destroying the pen lifts it, wherever it was.
                    self.device = None;
                    self.unavailable = true;
                    self.last = None;
                    return false;
                }
            }
        }
        self.last = Some(state);
        true
    }
}

fn pointer_info(s: &State) -> POINTER_TYPE_INFO {
    POINTER_TYPE_INFO {
        r#type: PT_PEN,
        Anonymous: POINTER_TYPE_INFO_0 {
            penInfo: POINTER_PEN_INFO {
                pointerInfo: POINTER_INFO {
                    pointerType: PT_PEN,
                    pointerFlags: POINTER_FLAGS(s.flags),
                    ptPixelLocation: POINT { x: s.at.0, y: s.at.1 },
                    ButtonChangeType: s.change,
                    ..Default::default()
                },
                penFlags: s.pen_flags,
                penMask: PEN_MASK_PRESSURE | PEN_MASK_ROTATION | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
                pressure: s.pressure,
                rotation: s.rotation,
                tiltX: s.tilt.0,
                tiltY: s.tilt.1,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    /// Hovers, draws a short stroke with the barrel button held for its last
    /// point, and leaves, at the middle of the main display.
    fn stroke() {
        let (cx, cy) = unsafe { (GetSystemMetrics(SM_CXSCREEN) / 2, GetSystemMetrics(SM_CYSCREEN) / 2) };
        let mut pen = PenInjector::default();
        let at = |dx: i32| (cx + dx, cy);
        let pen_at = |contact: bool, pressure: f32, barrel: bool| Pen {
            contact,
            pressure,
            tilt_x: 20,
            tilt_y: -10,
            twist: 45,
            barrel,
            ..Default::default()
        };
        let pause = || std::thread::sleep(Duration::from_millis(30));
        assert!(pen.update(&pen_at(false, 0.0, false), at(-40)));
        // Left still and hovering, it's sent again.
        std::thread::sleep(Duration::from_millis(150));
        pen.tick();
        pause();
        for (i, pressure) in [0.25, 0.5, 0.75, 1.0].into_iter().enumerate() {
            pen.update(&pen_at(true, pressure, false), at(-30 + 10 * i as i32));
            pause();
        }
        pen.update(&pen_at(false, 0.0, false), at(20));
        pause();
        pen.update(&pen_at(true, 0.5, true), at(30));
        pause();
        pen.update(&pen_at(false, 0.0, true), at(30));
        pause();
        // Hovers, then rubs out with the other end.
        pen.update(&pen_at(false, 0.0, false), at(40));
        pause();
        pen.update(&Pen { eraser: true, ..pen_at(true, 0.5, false) }, at(40));
        pause();
        pen.update(&Pen { eraser: true, ..pen_at(false, 0.0, false) }, at(40));
        pause();
        pen.leave();
        pause();
    }

    /// Draws on the real screen, so it only runs when asked for, with a window
    /// there to see it: `cargo test -p conductor-host pen::tests -- --ignored --test-threads 1`.
    #[test]
    #[ignore]
    fn draws() {
        stroke();
    }

    /// Conductor's own pen still works while this PC's keyboard and mouse are off.
    #[test]
    #[ignore]
    fn draws_while_blocked() {
        assert!(crate::block::set(true));
        stroke();
        crate::block::set(false);
    }
}
