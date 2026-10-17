//! App windows a session can show on their own, cut out of the display
//! they're on.
//!
//! Showing a window is a crop of its display, not a capture of the window
//! itself: whatever covers it is shown too, so the picture always matches
//! what clicks land on.

use conductor_proto::{DisplayInfo, WindowInfo};
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GWL_EXSTYLE, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    IsIconic, IsWindow, IsWindowVisible, SW_RESTORE, SetForegroundWindow, ShowWindow, SwitchToThisWindow,
    WS_EX_TOOLWINDOW,
};
use windows::core::BOOL;

use crate::indicator::Area;

/// A window, as a number so it can go to the pipeline thread.
pub type Handle = isize;

/// At most this many are listed, frontmost first.
const MOST: usize = 40;
/// The desktop, its wallpaper layer and the taskbar are windows too.
const NOT_APPS: [&str; 3] = ["Progman", "WorkerW", "Shell_TrayWnd"];
/// Smaller windows are shown with some of what's around them, since
/// encoders refuse tiny pictures.
const MIN_VIEW: i32 = 160;

fn hwnd(w: Handle) -> HWND {
    HWND(w as *mut _)
}

/// The windows someone could pick on this PC's desktop, frontmost first.
pub fn list() -> Vec<WindowInfo> {
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> BOOL {
        let found = unsafe { &mut *(found.0 as *mut Vec<HWND>) };
        if is_app(hwnd) {
            found.push(hwnd);
        }
        (found.len() < MOST).into()
    }
    let mut found: Vec<HWND> = Vec::new();
    // Fails when stopped early, which is fine.
    unsafe { _ = EnumWindows(Some(each), LPARAM(&mut found as *mut Vec<HWND> as isize)) };
    found.into_iter().filter_map(|h| Some(WindowInfo { id: (h.0 as Handle).to_string(), title: title(h)? })).collect()
}

/// A window someone would recognize as an app: shown, on the taskbar, with a title.
fn is_app(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid()) {
            return false;
        }
        if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
    }
    if cloaked(hwnd) || title(hwnd).is_none() {
        return false;
    }
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
    !NOT_APPS.contains(&String::from_utf16_lossy(&class[..len]).as_str())
}

/// Hidden by the system, like the windows of suspended Store apps or ones on another virtual desktop.
fn cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    let got = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4) };
    got.is_ok() && cloaked != 0
}

fn title(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
    let title = String::from_utf16_lossy(&buf[..len]).trim().to_string();
    (!title.is_empty()).then_some(title)
}

/// The window an id from [`list`] names, if it's still open.
pub fn from_id(id: &str) -> Option<Handle> {
    id.parse::<Handle>().ok().filter(|&w| unsafe { IsWindow(Some(hwnd(w))) }.as_bool())
}

pub fn info(w: Handle) -> Option<WindowInfo> {
    let h = hwnd(w);
    unsafe { IsWindow(Some(h)) }.as_bool().then(|| title(h)).flatten().map(|title| WindowInfo { id: w.to_string(), title })
}

/// Restores the window if it's minimized and puts it in front, so it isn't
/// covered when shown. Windows may refuse to change the foreground window.
pub fn bring_forward(w: Handle) {
    let h = hwnd(w);
    unsafe {
        if IsIconic(h).as_bool() {
            _ = ShowWindow(h, SW_RESTORE);
        }
        if !SetForegroundWindow(h).as_bool() {
            SwitchToThisWindow(h, true);
        }
    }
}

pub fn minimized(w: Handle) -> bool {
    unsafe { IsIconic(hwnd(w)) }.as_bool()
}

/// Where the window is on the desktop, in physical pixels, while it's open,
/// shown and not minimized.
pub fn rect(w: Handle) -> Option<RECT> {
    let h = hwnd(w);
    unsafe {
        if !IsWindow(Some(h)).as_bool() || !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
        // Without the invisible borders Windows adds for resizing.
        let mut r = RECT::default();
        let size = size_of::<RECT>() as u32;
        if DwmGetWindowAttribute(h, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, size).is_err() {
            GetWindowRect(h, &mut r).ok()?;
        }
        Some(r)
    }
}

/// The display most of the window is on, and the part of it to show.
pub fn place(w: Handle, displays: &[DisplayInfo]) -> Option<(String, Area)> {
    let r = rect(w)?;
    let (display, view) = displays
        .iter()
        .filter_map(|d| {
            let area = Area { x: d.x, y: d.y, width: d.width, height: d.height };
            Some((d, crop(r, area)?, overlap(r, area)))
        })
        .max_by_key(|&(_, _, overlap)| overlap)
        .map(|(d, view, _)| (d.id.clone(), view))?;
    Some((display, view))
}

fn overlap(r: RECT, a: Area) -> i64 {
    let w = r.right.min(a.x + a.width as i32) - r.left.max(a.x);
    let h = r.bottom.min(a.y + a.height as i32) - r.top.max(a.y);
    w.max(0) as i64 * h.max(0) as i64
}

/// The part of `display` that shows `window`: grown to a usable size when the
/// window is tiny, and even in both directions for the encoder. None when the
/// window isn't on the display.
pub fn crop(window: RECT, display: Area) -> Option<Area> {
    let (left, top) = (display.x, display.y);
    let (right, bottom) = (left + display.width as i32, top + display.height as i32);
    let (mut l, mut t) = (window.left.max(left), window.top.max(top));
    let (mut r, mut b) = (window.right.min(right), window.bottom.min(bottom));
    if r <= l || b <= t {
        return None;
    }
    grow(&mut l, &mut r, left, right);
    grow(&mut t, &mut b, top, bottom);
    r -= (r - l) & 1;
    b -= (b - t) & 1;
    Some(Area { x: l, y: t, width: (r - l) as u32, height: (b - t) as u32 })
}

/// Widens `lo..hi` to [`MIN_VIEW`] around its middle, staying within `start..end`.
fn grow(lo: &mut i32, hi: &mut i32, start: i32, end: i32) {
    let short = MIN_VIEW - (*hi - *lo);
    if short <= 0 {
        return;
    }
    *lo = (*lo - short / 2).max(start);
    *hi = (*lo + MIN_VIEW).min(end);
    *lo = (*hi - MIN_VIEW).max(start);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT { left, top, right, bottom }
    }

    const SCREEN: Area = Area { x: 0, y: 0, width: 1920, height: 1080 };

    #[test]
    fn crops_to_the_window() {
        assert_eq!(crop(rect(100, 50, 901, 651), SCREEN), Some(Area { x: 100, y: 50, width: 800, height: 600 }));
    }

    #[test]
    fn trims_to_the_display() {
        let left_of_it = Area { x: -1280, y: 0, width: 1280, height: 1024 };
        assert_eq!(crop(rect(-200, 100, 600, 500), SCREEN), Some(Area { x: 0, y: 100, width: 600, height: 400 }));
        assert_eq!(crop(rect(-200, 100, 600, 500), left_of_it), Some(Area { x: -200, y: 100, width: 200, height: 400 }));
        assert_eq!(crop(rect(2000, 100, 2600, 500), SCREEN), None);
    }

    #[test]
    fn grows_tiny_windows_within_the_display() {
        assert_eq!(crop(rect(500, 500, 540, 520), SCREEN), Some(Area { x: 440, y: 430, width: 160, height: 160 }));
        assert_eq!(crop(rect(0, 0, 40, 20), SCREEN), Some(Area { x: 0, y: 0, width: 160, height: 160 }));
        assert_eq!(crop(rect(1900, 1070, 1920, 1080), SCREEN), Some(Area { x: 1760, y: 920, width: 160, height: 160 }));
    }

    #[test]
    fn picks_the_display_with_most_of_the_window() {
        let r = rect(1800, 100, 2200, 500);
        let right = Area { x: 1920, y: 0, width: 1920, height: 1080 };
        assert!(overlap(r, right) > overlap(r, SCREEN));
    }
}
