//! Keeps this PC awake while someone controls it, and puts it to sleep,
//! restarts or shuts it down when asked.

use std::time::Duration;

use anyhow::{Result, bail};
use conductor_proto::PowerAction;
use windows::Win32::Foundation::{CloseHandle, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::{
    POWER_REQUEST_TYPE, PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired, PowerRequestSystemRequired,
    PowerSetRequest, SetSuspendState,
};
use windows::Win32::System::Shutdown::{
    EWX_FORCEIFHUNG, EWX_POWEROFF, EWX_REBOOT, ExitWindowsEx, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
};
use windows::core::PWSTR;

const KINDS: [POWER_REQUEST_TYPE; 2] = [PowerRequestSystemRequired, PowerRequestDisplayRequired];

/// Keeps the PC and its displays on while it's alive. A PC woken over the
/// network would otherwise go back to sleep soon after, and a screen someone
/// is only watching would turn off.
pub struct KeepAwake(HANDLE);

// A power request handle isn't tied to the thread that made it.
unsafe impl Send for KeepAwake {}

impl KeepAwake {
    pub fn new(controller: &str) -> Option<Self> {
        // Shown by `powercfg /requests`.
        let mut reason: Vec<u16> = format!("Controlled from {controller}").encode_utf16().chain(Some(0)).collect();
        let context = REASON_CONTEXT {
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 { SimpleReasonString: PWSTR(reason.as_mut_ptr()) },
        };
        unsafe {
            let request = match PowerCreateRequest(&context) {
                Ok(request) => request,
                Err(e) => {
                    tracing::warn!("keeping this PC awake: {e}");
                    return None;
                }
            };
            for kind in KINDS {
                if let Err(e) = PowerSetRequest(request, kind) {
                    tracing::warn!("keeping this PC awake: {e}");
                }
            }
            Some(Self(request))
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        unsafe {
            for kind in KINDS {
                _ = PowerClearRequest(self.0, kind);
            }
            _ = CloseHandle(self.0);
        }
    }
}

/// Sleeps, restarts or shuts down this PC a moment from now, so the reply to
/// whoever asked gets out first. Apps with unsaved work can hold up a restart
/// or shutdown, as they would from the Start menu.
pub fn act(action: PowerAction) -> Result<()> {
    allow_shutdown()?;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        let flags = EWX_FORCEIFHUNG;
        let reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED;
        let result = unsafe {
            match action {
                // Wake events stay on, or the PC couldn't be woken over the network.
                PowerAction::Sleep => {
                    if SetSuspendState(false, false, false) { Ok(()) } else { Err(windows::core::Error::from_thread()) }
                }
                PowerAction::Restart => ExitWindowsEx(EWX_REBOOT | flags, reason),
                PowerAction::ShutDown => ExitWindowsEx(EWX_POWEROFF | flags, reason),
            }
        };
        if let Err(e) = result {
            tracing::warn!("{action:?}: {e}");
        }
    });
    Ok(())
}

/// Turns on the privilege that sleeping and shutting down need.
fn allow_shutdown() -> Result<()> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)?;
        let mut luid = LUID::default();
        let result = LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid).and_then(|()| {
            let privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None)
        });
        // AdjustTokenPrivileges succeeds without the privilege when the account doesn't have it.
        let missing = result.is_ok() && GetLastError() == ERROR_NOT_ALL_ASSIGNED;
        _ = CloseHandle(token);
        result?;
        if missing {
            bail!("This account isn't allowed to shut down the PC.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn may_shut_down() {
        // Every Windows desktop account has the privilege; only turning it on is tested.
        super::allow_shutdown().unwrap();
    }
}
