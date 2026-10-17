//! Turns off this PC's own keyboard and mouse while a session hides its
//! screen, so nobody at the PC can get in the way of what they can't see.
//!
//! Low-level hooks drop input that comes from the devices themselves and let
//! through input that programs send, which is how Conductor's own input
//! arrives. Windows never lets a hook block Ctrl+Alt+Del, and touch screens and
//! pens still reach apps that read them directly.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLKHF_UP, LLMHF_INJECTED, MSG,
    MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_QUIT,
};

/// The hook thread's ID, and the thread.
static THREAD: Mutex<Option<(u32, JoinHandle<()>)>> = Mutex::new(None);
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// Starts or stops blocking. Returns whether input is now blocked.
pub fn set(on: bool) -> bool {
    let mut thread = THREAD.lock().unwrap();
    if on && thread.is_none() {
        *thread = start();
    } else if !on {
        if let Some((id, handle)) = thread.take() {
            _ = unsafe { PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0)) };
            _ = handle.join();
        }
    }
    thread.is_some()
}

fn start() -> Option<(u32, JoinHandle<()>)> {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("block input".into()).spawn(move || unsafe {
        // Create this thread's message queue before announcing it.
        let mut msg = MSG::default();
        _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let instance = GetModuleHandleW(None).ok();
        let hooks = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard), instance.map(Into::into), 0)
            .and_then(|k| match SetWindowsHookExW(WH_MOUSE_LL, Some(mouse), instance.map(Into::into), 0) {
                Ok(m) => Ok((k, m)),
                Err(e) => {
                    _ = UnhookWindowsHookEx(k);
                    Err(e)
                }
            });
        let (k, m) = match hooks {
            Ok(hooks) => hooks,
            Err(e) => {
                _ = tx.send(Err(e));
                return;
            }
        };
        _ = tx.send(Ok(GetCurrentThreadId()));
        DROPPED.store(0, Ordering::Relaxed);
        tracing::info!("this PC's keyboard and mouse are off");
        // Hooks are called from this loop, so it does nothing else.
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
        _ = UnhookWindowsHookEx(k);
        _ = UnhookWindowsHookEx(m);
        tracing::info!("this PC's keyboard and mouse are back on; ignored {} inputs", DROPPED.load(Ordering::Relaxed));
    });
    let handle = match spawned {
        Ok(handle) => handle,
        Err(e) => {
            tracing::warn!("blocking input: {e}");
            return None;
        }
    };
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(id)) => Some((id, handle)),
        Ok(Err(e)) => {
            tracing::warn!("blocking input: {e}");
            None
        }
        Err(_) => {
            tracing::warn!("blocking input: the hook thread didn't start");
            None
        }
    }
}

/// Whether a key event from the keyboard should be dropped. Releasing a key
/// that's down still goes through, so nothing stays held from before.
fn drop_key(flags: u32, key_down: bool) -> bool {
    flags & LLKHF_INJECTED.0 == 0 && (flags & LLKHF_UP.0 == 0 || !key_down)
}

unsafe extern "system" fn keyboard(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = unsafe { GetAsyncKeyState(info.vkCode as i32) } < 0;
        if drop_key(info.flags.0, down) {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0 {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        let (injected, up) = (LLKHF_INJECTED.0, LLKHF_UP.0);
        // From the keyboard: pressing is dropped, and so is releasing a key that isn't down.
        assert!(drop_key(0, false));
        assert!(drop_key(0, true));
        assert!(drop_key(up, false));
        // Releasing a key that's down goes through.
        assert!(!drop_key(up, true));
        // Sent by a program, such as Conductor: always goes through.
        assert!(!drop_key(injected, false));
        assert!(!drop_key(injected | up, false));
    }

    #[test]
    fn starts_and_stops() {
        assert!(set(true));
        assert!(set(true));
        assert!(!set(false));
        assert!(!set(false));
    }

    /// Moves the real pointer, so it only runs when asked for:
    /// `cargo test -p conductor-host sent_input_gets_through -- --ignored`.
    #[test]
    #[ignore]
    fn sent_input_gets_through() {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE, MOUSEINPUT, SendInput,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

        let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
        let mut before = POINT::default();
        unsafe { GetCursorPos(&mut before) }.unwrap();
        let target = if before.x < w / 2 { (w * 3 / 4, h / 2) } else { (w / 4, h / 2) };
        let move_to = |(x, y): (i32, i32)| {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: x * 65535 / (w - 1),
                        dy: y * 65535 / (h - 1),
                        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
                        ..Default::default()
                    },
                },
            };
            unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
            std::thread::sleep(Duration::from_millis(200));
            let mut now = POINT::default();
            unsafe { GetCursorPos(&mut now) }.unwrap();
            (now.x, now.y)
        };

        assert!(set(true));
        let moved = move_to(target);
        let back = move_to((before.x, before.y));
        set(false);
        assert!((moved.0 - target.0).abs() <= 2 && (moved.1 - target.1).abs() <= 2, "{moved:?} != {target:?}");
        assert!((back.0 - before.x).abs() <= 2, "{back:?}");
    }
}
