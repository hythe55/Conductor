//! Keeps this PC awake while someone controls it.

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Power::{
    POWER_REQUEST_TYPE, PowerClearRequest, PowerCreateRequest, PowerRequestDisplayRequired, PowerRequestSystemRequired,
    PowerSetRequest,
};
use windows::Win32::System::Threading::{POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0};
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
