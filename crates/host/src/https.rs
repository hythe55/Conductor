//! A small blocking HTTPS GET on WinHTTP, so updates use the system's TLS,
//! certificate store and proxy settings.

use std::ffi::c_void;

use anyhow::{Context, Result, bail};
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

const TIMEOUT_MS: i32 = 30_000;

/// The server answered with something other than 200 OK.
#[derive(Debug)]
pub struct HttpStatus(pub u32);

impl std::fmt::Display for HttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.0)
    }
}

impl std::error::Error for HttpStatus {}

struct Handle(*mut c_void);

impl Handle {
    fn new(h: *mut c_void, what: &str) -> Result<Self> {
        if h.is_null() {
            return Err(windows::core::Error::from_thread()).context(what.to_string());
        }
        Ok(Self(h))
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { _ = WinHttpCloseHandle(self.0) };
    }
}

/// Fetches `url` and returns the body. Only `https://` URLs are allowed, and
/// redirects never drop down to plain HTTP. Bodies over `limit` bytes fail.
pub fn get(url: &str, limit: usize) -> Result<Vec<u8>> {
    let rest = url.strip_prefix("https://").context("only HTTPS URLs are allowed")?;
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    if host.is_empty() || host.contains(['@', ':']) {
        bail!("unsupported URL {url}");
    }
    let path = if path.is_empty() { "/" } else { path };

    unsafe {
        let session = Handle::new(
            WinHttpOpen(
                &HSTRING::from(format!("Conductor/{}", env!("CARGO_PKG_VERSION"))),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS)?;
        let connection = Handle::new(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0), "connecting")?;
        let request = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(path),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE | WINHTTP_OPEN_REQUEST_FLAGS(0),
            ),
            "opening request",
        )?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).with_context(|| format!("requesting {url}"))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).with_context(|| format!("waiting for {url}"))?;

        let mut code = 0u32;
        let mut size = size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut code as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )?;
        if code != 200 {
            return Err(anyhow::Error::new(HttpStatus(code)).context(format!("fetching {url}")));
        }

        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(request.0, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u32, &mut read)
                .with_context(|| format!("reading {url}"))?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read as usize]);
            if body.len() > limit {
                bail!("{url} is larger than expected");
            }
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_plain_http() {
        assert!(get("http://example.com/", 1024).is_err());
        assert!(get("https://user@example.com/", 1024).is_err());
    }

    /// Needs the internet: `cargo test -p conductor-host -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_with_redirects_and_limits() {
        // github.com redirects the old repository name to the new one.
        assert!(get("https://github.com/hythe55/Connect", 8 * 1024 * 1024).is_ok());
        assert!(get("https://github.com/hythe55/Conductor", 16).is_err());
        let missing = get("https://github.com/hythe55/Conductor/releases/download/v0.0.0/none.json", 1024);
        assert_eq!(missing.unwrap_err().downcast_ref::<HttpStatus>().map(|s| s.0), Some(404));
    }
}
