//! Announces this PC to the networks it's on, so the phone app can find it
//! without being told its name (see `conductor_proto::nearby`). What goes out
//! is the PC's name and its tailnet address, which only the owner's devices
//! can reach. Windows answers for the PC's name on these networks already. The
//! owner can turn it off in Settings.

use std::ffi::c_void;
use std::net::IpAddr;
use std::time::Duration;

use conductor_proto::DEFAULT_PORT;
use conductor_proto::nearby::{SERVICE, TXT_ADDRESS};
use windows::Win32::Foundation::{DNS_REQUEST_PENDING, HANDLE};
use windows::Win32::NetworkManagement::Dns::{
    DNS_QUERY_REQUEST_VERSION1, DNS_SERVICE_INSTANCE, DNS_SERVICE_REGISTER_REQUEST, DnsServiceConstructInstance,
    DnsServiceDeRegister, DnsServiceFreeInstance, DnsServiceRegister,
};
use windows::core::{HSTRING, PCWSTR};

use crate::settings::Settings;

/// How soon turning it on or off in Settings takes effect.
const RECHECK: Duration = Duration::from_secs(10);

/// Announces `name` at `address`, while Settings says to, for as long as the agent runs.
pub fn spawn(name: String, host: String, address: IpAddr) {
    std::thread::spawn(move || {
        let mut announced = None;
        let mut failed = false;
        loop {
            if !Settings::load().nearby {
                announced = None;
            } else if announced.is_none() {
                match Announcement::new(&name, &host, address) {
                    Ok(a) => {
                        announced = Some(a);
                        failed = false;
                    }
                    Err(status) if !failed => {
                        tracing::warn!("can't announce this PC to nearby phones: {status}");
                        failed = true;
                    }
                    Err(_) => {}
                }
            }
            std::thread::sleep(RECHECK);
        }
    });
}

/// This PC, announced until dropped.
struct Announcement {
    request: DNS_SERVICE_REGISTER_REQUEST,
}

impl Announcement {
    fn new(name: &str, host: &str, address: IpAddr) -> Result<Self, u32> {
        let instance = HSTRING::from(format!("{}.{SERVICE}", label(name)));
        let host = HSTRING::from(format!("{}.local", label(host)));
        let key = HSTRING::from(TXT_ADDRESS);
        let value = HSTRING::from(address.to_string());
        let keys = [PCWSTR(key.as_ptr())];
        let values = [PCWSTR(value.as_ptr())];
        let (keys, values) = (keys.as_ptr(), values.as_ptr());
        let instance =
            unsafe { DnsServiceConstructInstance(&instance, &host, None, None, DEFAULT_PORT, 0, 0, 1, keys, values) };
        if instance.is_null() {
            return Err(0);
        }
        let request = DNS_SERVICE_REGISTER_REQUEST {
            Version: DNS_QUERY_REQUEST_VERSION1.0,
            // Every interface, including ones that come up later.
            InterfaceIndex: 0,
            pServiceInstance: instance,
            pRegisterCompletionCallback: Some(registered),
            pQueryContext: std::ptr::null_mut(),
            hCredentials: HANDLE::default(),
            unicastEnabled: false.into(),
        };
        let status = unsafe { DnsServiceRegister(&request, None) };
        if status != DNS_REQUEST_PENDING as u32 {
            unsafe { DnsServiceFreeInstance(instance) };
            return Err(status);
        }
        tracing::info!("announcing this PC to nearby phones");
        Ok(Self { request })
    }
}

impl Drop for Announcement {
    fn drop(&mut self) {
        // The instance is needed until Windows is done, so it's freed when it says so.
        let request = DNS_SERVICE_REGISTER_REQUEST {
            pRegisterCompletionCallback: Some(deregistered),
            pQueryContext: self.request.pServiceInstance as *mut c_void,
            ..self.request
        };
        let status = unsafe { DnsServiceDeRegister(&request, None) };
        if status != DNS_REQUEST_PENDING as u32 {
            unsafe { DnsServiceFreeInstance(self.request.pServiceInstance) };
        }
        tracing::info!("stopped announcing this PC to nearby phones");
    }
}

unsafe extern "system" fn registered(status: u32, _: *const c_void, instance: *const DNS_SERVICE_INSTANCE) {
    if status != 0 {
        tracing::warn!("announcing this PC to nearby phones failed: {status}");
    }
    if !instance.is_null() {
        unsafe { DnsServiceFreeInstance(instance) };
    }
}

unsafe extern "system" fn deregistered(_: u32, context: *const c_void, instance: *const DNS_SERVICE_INSTANCE) {
    unsafe {
        if !instance.is_null() {
            DnsServiceFreeInstance(instance);
        }
        DnsServiceFreeInstance(context as *const DNS_SERVICE_INSTANCE);
    }
}

/// `name` as one DNS label: no dots, and at most 63 bytes.
fn label(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars().map(|c| if c == '.' { '-' } else { c }) {
        if out.len() + c.len_utf8() > 63 {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(label("Studio PC"), "Studio PC");
        assert_eq!(label("a.b"), "a-b");
        assert_eq!(label(&"é".repeat(40)).len(), 62);
    }

    /// Announces a test PC and looks for it the way the phone app does:
    /// `cargo test -p conductor-host finds_itself -- --ignored`.
    #[test]
    #[ignore = "announces on the local network"]
    fn finds_itself() {
        let address: IpAddr = "100.64.0.7".parse().unwrap();
        let announced = Announcement::new("conductor-test", "conductor-test", address).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        let found = conductor_proto::nearby::find(Duration::from_millis(1500)).unwrap();
        drop(announced);
        std::thread::sleep(Duration::from_secs(1));
        assert!(found.iter().any(|pc| pc.name == "conductor-test" && pc.address == "100.64.0.7"), "{found:?}");
    }
}
