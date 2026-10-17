//! Follows the desktop that currently receives input.
//!
//! Windows shows the lock screen, sign-in screen and UAC prompts on a separate
//! secure desktop. Screen capture and SendInput only work on the desktop the
//! calling thread is attached to, so when either stops working we re-attach to
//! the input desktop and try again. This only succeeds when running as SYSTEM
//! (the service-launched agent); a normal user process simply can't see the
//! secure desktop.

use std::cell::Cell;

use windows::Win32::Foundation::GENERIC_ALL;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, HDESK, OpenInputDesktop, SetThreadDesktop,
};

thread_local! {
    static ATTACHED: Cell<Option<HDESK>> = const { Cell::new(None) };
}

/// Attaches the calling thread to the current input desktop.
/// Returns true if the thread is now on it.
pub fn follow_input() -> bool {
    unsafe {
        let Ok(desk) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(GENERIC_ALL.0)) else {
            return false;
        };
        if SetThreadDesktop(desk).is_err() {
            _ = CloseDesktop(desk);
            return false;
        }
        // The previous desktop handle can only be closed once no thread uses it.
        if let Some(old) = ATTACHED.replace(Some(desk)) {
            _ = CloseDesktop(old);
        }
        true
    }
}
