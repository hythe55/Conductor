//! Asks the person at this PC whether a device may control it, when the owner
//! has turned that on. A plain Windows dialog on the signed-in user's desktop
//! that says no by itself when nobody answers. While the PC is locked nobody
//! sees it, so nobody gets in.

use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use conductor_proto::ASK_WAIT_SECS;
use windows::Win32::Foundation::{HWND, LPARAM, S_OK, WPARAM};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TDE_FOOTER, TDF_CALLBACK_TIMER, TDM_CLICK_BUTTON,
    TDM_UPDATE_ELEMENT_TEXT, TDN_CREATED, TDN_TIMER,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SendMessageW, SetForegroundWindow, SetWindowPos,
};
use windows::core::{BOOL, HRESULT, HSTRING, PCWSTR, s, w};

/// How long the question waits for an answer.
const WAIT: Duration = Duration::from_secs(ASK_WAIT_SECS);
const ALLOW: i32 = 100;
const DECLINE: i32 = 101;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Allowed,
    Declined,
    Unanswered,
}

/// Devices being asked about right now.
static ASKING: Mutex<Vec<IpAddr>> = Mutex::new(Vec::new());

/// Takes `peer` off [`ASKING`], also when the request is given up on.
struct Asking(IpAddr);

impl Drop for Asking {
    fn drop(&mut self) {
        ASKING.lock().unwrap().retain(|p| *p != self.0);
    }
}

/// Asks whether `controller`, at `peer`, may control this PC. None when that's
/// being asked already.
pub async fn ask(controller: String, peer: IpAddr) -> Option<Answer> {
    {
        let mut asking = ASKING.lock().unwrap();
        if asking.contains(&peer) {
            return None;
        }
        asking.push(peer);
    }
    let _asking = Asking(peer);
    Some(tokio::task::spawn_blocking(move || show(&controller)).await.unwrap_or(Answer::Unanswered))
}

type TaskDialogIndirect = unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut BOOL) -> HRESULT;

/// Found at run time, so that without common controls 6 (which the manifest
/// asks for) Conductor still starts and only this goes without.
fn task_dialog() -> Option<TaskDialogIndirect> {
    unsafe {
        let module = LoadLibraryW(w!("comctl32.dll")).ok()?;
        let f = GetProcAddress(module, s!("TaskDialogIndirect"))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, TaskDialogIndirect>(f))
    }
}

fn footer(left: u64) -> HSTRING {
    let unit = if left == 1 { "second" } else { "seconds" };
    HSTRING::from(format!("If nobody answers in {left} {unit}, they won’t be allowed in."))
}

struct Countdown {
    left: u64,
    timed_out: bool,
}

fn show(controller: &str) -> Answer {
    let Some(task_dialog) = task_dialog() else {
        tracing::warn!("can't ask whether {controller} may connect: no task dialogs on this PC");
        return Answer::Unanswered;
    };
    let instruction = HSTRING::from(format!("{controller} wants to control this PC"));
    let first_footer = footer(WAIT.as_secs());
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: ALLOW, pszButtonText: w!("Allow") },
        TASKDIALOG_BUTTON { nButtonID: DECLINE, pszButtonText: w!("Don’t allow") },
    ];
    let mut countdown = Countdown { left: WAIT.as_secs(), timed_out: false };
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_CALLBACK_TIMER,
        pszWindowTitle: w!("Conductor"),
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: w!("If you allow it, they’ll see this screen and can use its keyboard and mouse."),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        // Enter, pressed while typing something else, shouldn't let anyone in.
        nDefaultButton: DECLINE,
        pszFooter: PCWSTR(first_footer.as_ptr()),
        pfCallback: Some(callback),
        lpCallbackData: &mut countdown as *mut Countdown as isize,
        ..Default::default()
    };
    let mut pressed = 0;
    if let Err(e) = unsafe { task_dialog(&config, &mut pressed, std::ptr::null_mut(), std::ptr::null_mut()) }.ok() {
        tracing::warn!("asking whether {controller} may connect: {e}");
        return Answer::Unanswered;
    }
    match pressed {
        _ if countdown.timed_out => Answer::Unanswered,
        ALLOW => Answer::Allowed,
        _ => Answer::Declined,
    }
}

unsafe extern "system" fn callback(
    hwnd: HWND,
    msg: TASKDIALOG_NOTIFICATIONS,
    wparam: WPARAM,
    _: LPARAM,
    data: isize,
) -> HRESULT {
    let countdown = unsafe { &mut *(data as *mut Countdown) };
    match msg {
        TDN_CREATED => unsafe {
            // Nobody at this PC asked for it, so it goes over whatever they're doing.
            _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
            _ = SetForegroundWindow(hwnd);
        },
        TDN_TIMER => {
            // Milliseconds since the dialog opened.
            let left = WAIT.as_secs().saturating_sub(wparam.0 as u64 / 1000);
            if left != countdown.left {
                countdown.left = left;
                if left == 0 {
                    countdown.timed_out = true;
                    unsafe { SendMessageW(hwnd, TDM_CLICK_BUTTON.0 as u32, Some(WPARAM(DECLINE as usize)), None) };
                } else {
                    let text = footer(left);
                    let lparam = LPARAM(text.as_ptr() as isize);
                    unsafe {
                        SendMessageW(hwnd, TDM_UPDATE_ELEMENT_TEXT.0 as u32, Some(WPARAM(TDE_FOOTER.0 as usize)), Some(lparam))
                    };
                }
            }
        }
        _ => {}
    }
    S_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shows the question on this PC's screen: `cargo test -p conductor-host asks -- --ignored`.
    #[test]
    #[ignore = "opens a dialog on the screen"]
    fn asks() {
        assert!(task_dialog().is_some(), "common controls 6 missing; is the manifest embedded?");
        println!("{:?}", show("conductor-test-device"));
    }
}
