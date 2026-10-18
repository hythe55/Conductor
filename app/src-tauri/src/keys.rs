//! While the screen of the PC being controlled has focus, shortcuts like
//! Alt+Tab and the Windows key go to that PC instead of acting on this one.
//! Windows acts on them before the app sees them, so a low-level keyboard
//! hook takes them first and hands them to the app.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::ipc::Channel;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::local::Problem;

/// A key taken from Windows, for the app to send to the other PC.
#[derive(Clone, Serialize)]
pub struct Key {
    code: String,
    down: bool,
}

/// The request the page asked with and the thread running the hook, while shortcuts are taken.
static HOOK_THREAD: Mutex<Option<(u32, u32)>> = Mutex::new(None);

/// Keys pressed on this PC since shortcuts were last taken, and how many of them were kept from Windows.
static SEEN: AtomicU32 = AtomicU32::new(0);
static KEPT: AtomicU32 = AtomicU32::new(0);

const COULDNT: &str = "Couldn't send Windows shortcuts to the other PC.";

/// Starts taking shortcuts from Windows while `window` is in front, giving them to `keys`.
/// `request` is the page's id for the request, to let go of them with.
#[tauri::command]
pub fn capture_keys(window: tauri::WebviewWindow, keys: Channel<Key>, request: u32) -> Result<(), Problem> {
    stop();
    SEEN.store(0, Ordering::Relaxed);
    KEPT.store(0, Ordering::Relaxed);
    let window = window.hwnd().map_err(|e| Problem::new(COULDNT, e))?.0 as isize;
    let (started_tx, started) = std::sync::mpsc::channel();
    // Sending to the page waits for the app's main thread, and Windows lets a
    // key through and quietly drops a hook that takes too long, so the hook
    // only queues keys and this thread sends them on.
    let (taken_tx, taken) = std::sync::mpsc::channel::<Key>();
    std::thread::Builder::new()
        .name("shortcut keys".into())
        .spawn(move || {
            for key in taken {
                _ = keys.send(key);
            }
        })
        .map_err(|e| Problem::new(COULDNT, e))?;
    // Its own thread, since Windows drops a hook that's slow to answer.
    std::thread::Builder::new()
        .name("shortcuts".into())
        .spawn(move || unsafe {
            _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            // Makes this thread's message queue, so it can be told to stop.
            let mut msg = MSG::default();
            _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            // Keys already down when this starts.
            let held = std::array::from_fn(|vk| GetAsyncKeyState(vk as i32) < 0);
            let taking = Taking { window: HWND(window as _), keys: taken_tx, held, taken: [false; 256] };
            TAKING.with(|t| *t.borrow_mut() = Some(taking));
            let module = GetModuleHandleW(None).ok().map(Into::into);
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), module, 0) {
                Ok(hook) => hook,
                Err(e) => {
                    _ = started_tx.send(Err(e));
                    return;
                }
            };
            _ = started_tx.send(Ok(GetCurrentThreadId()));
            // The hook is called while this waits for messages.
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
            _ = UnhookWindowsHookEx(hook);
        })
        .map_err(|e| Problem::new(COULDNT, e))?;
    let thread = started
        .recv()
        .map_err(|e| Problem::new(COULDNT, e))?
        .map_err(|e| Problem::new(COULDNT, e))?;
    *HOOK_THREAD.lock().unwrap() = Some((request, thread));
    Ok(())
}

/// Leaves shortcuts to Windows again, if they're still taken for `request`.
/// The page can ask for them again before an older request to let go arrives,
/// which then mustn't let go of the newer ones.
#[tauri::command]
pub fn release_keys(request: u32) {
    let mut taken = HOOK_THREAD.lock().unwrap();
    if taken.is_some_and(|(asked, _)| asked == request) {
        quit(taken.take());
    }
}

/// Whether shortcuts are taken, for the connection details.
#[derive(Serialize)]
pub struct ShortcutStatus {
    taking: bool,
    seen: u32,
    kept: u32,
}

#[tauri::command]
pub fn shortcut_status() -> ShortcutStatus {
    ShortcutStatus {
        taking: HOOK_THREAD.lock().unwrap().is_some(),
        seen: SEEN.load(Ordering::Relaxed),
        kept: KEPT.load(Ordering::Relaxed),
    }
}

fn stop() {
    quit(HOOK_THREAD.lock().unwrap().take());
}

fn quit(hook: Option<(u32, u32)>) {
    if let Some((_, thread)) = hook {
        unsafe { _ = PostThreadMessageW(thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}

struct Taking {
    /// The app's window. Shortcuts are only taken while it's in front.
    window: HWND,
    keys: std::sync::mpsc::Sender<Key>,
    /// Keys held down, whether Windows saw them or not.
    held: [bool; 256],
    /// Keys whose press was taken, so letting go of them is taken too.
    taken: [bool; 256],
}

thread_local! {
    static TAKING: RefCell<Option<Taking>> = const { RefCell::new(None) };
}

unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Typed by the host on this PC, say while it controls itself.
        let typed = info.dwExtraInfo == conductor_proto::keys::TYPED;
        let take = |t: &RefCell<Option<Taking>>| {
            t.try_borrow_mut().is_ok_and(|mut t| t.as_mut().is_some_and(|t| t.take(info)))
        };
        if !typed {
            // Presses only, for the connection details.
            let down = !info.flags.contains(LLKHF_UP);
            SEEN.fetch_add(down as u32, Ordering::Relaxed);
            if TAKING.with(take) {
                KEPT.fetch_add(down as u32, Ordering::Relaxed);
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

impl Taking {
    /// Whether to keep this key from Windows, sending it to the app if so.
    fn take(&mut self, info: &KBDLLHOOKSTRUCT) -> bool {
        let vk = (info.vkCode & 0xFF) as usize;
        let down = !info.flags.contains(LLKHF_UP);
        self.held[vk] = down;
        // Windows sees a key go up only if it saw it go down.
        let take = self.taken[vk] || (down && self.shortcut(VIRTUAL_KEY(vk as u16), info.flags));
        if !take {
            return false;
        }
        self.taken[vk] = down;
        if let Some(code) = code(info) {
            _ = self.keys.send(Key { code, down });
        }
        true
    }

    /// Whether Windows would act on this key itself.
    fn shortcut(&self, vk: VIRTUAL_KEY, flags: KBDLLHOOKSTRUCT_FLAGS) -> bool {
        if unsafe { GetForegroundWindow() } != self.window {
            return false;
        }
        let held = |k: VIRTUAL_KEY| self.held[k.0 as usize];
        let alt = flags.contains(LLKHF_ALTDOWN);
        let ctrl = held(VK_LCONTROL) || held(VK_RCONTROL);
        let shift = held(VK_LSHIFT) || held(VK_RSHIFT);
        let windows_key = self.taken[VK_LWIN.0 as usize] || self.taken[VK_RWIN.0 as usize];
        matches!(vk, VK_LWIN | VK_RWIN)
            // Everything pressed with the Windows key, like Windows+D.
            || windows_key
            // Alt+Tab, Alt+Esc, Alt+F4 and the window menu.
            || (alt && matches!(vk, VK_TAB | VK_ESCAPE | VK_F4 | VK_SPACE))
            // Start. Ctrl+Shift+Esc opens Task Manager here.
            || (ctrl && !shift && vk == VK_ESCAPE)
    }
}

/// The DOM code of a key, which the other PC takes.
fn code(info: &KBDLLHOOKSTRUCT) -> Option<String> {
    let code = match VIRTUAL_KEY(info.vkCode as u16) {
        // Windows marks these differently from the keyboard.
        VK_LSHIFT => "ShiftLeft",
        VK_RSHIFT => "ShiftRight",
        VK_NUMLOCK => "NumLock",
        VK_PAUSE => return None,
        _ => return conductor_proto::keys::code(info.scanCode as u16, info.flags.contains(LLKHF_EXTENDED)),
    };
    Some(code.into())
}
