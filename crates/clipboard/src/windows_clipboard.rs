use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

/// Changes whenever anything puts something new on the clipboard.
pub fn sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// The clipboard is open; it closes when this is dropped.
struct Open;

impl Open {
    fn new() -> Result<Self> {
        // Another app may have it open for a moment.
        for _ in 0..10 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Open);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        bail!("the clipboard is in use by another app")
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        unsafe { _ = CloseClipboard() };
    }
}

/// The text on the clipboard, or None when it holds something else or more
/// than `max_bytes` of text.
pub fn read_text(max_bytes: usize) -> Result<Option<String>> {
    let format = u32::from(CF_UNICODETEXT.0);
    unsafe {
        if IsClipboardFormatAvailable(format).is_err() {
            return Ok(None);
        }
        let _open = Open::new()?;
        let Ok(handle) = GetClipboardData(format) else { return Ok(None) };
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        if size > max_bytes.saturating_mul(2) {
            return Ok(None);
        }
        let ptr = GlobalLock(memory) as *const u16;
        if ptr.is_null() {
            return Ok(None);
        }
        let units = std::slice::from_raw_parts(ptr, size / 2);
        let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
        let text = String::from_utf16_lossy(&units[..end]);
        _ = GlobalUnlock(memory);
        Ok((text.len() <= max_bytes).then_some(text))
    }
}

/// The files copied in Explorer (or anything else that copies files), or None
/// when the clipboard holds something else.
pub fn read_files() -> Result<Option<Vec<PathBuf>>> {
    let format = u32::from(CF_HDROP.0);
    unsafe {
        if IsClipboardFormatAvailable(format).is_err() {
            return Ok(None);
        }
        let _open = Open::new()?;
        let handle = GetClipboardData(format).context("reading the copied files")?;
        let drop = HDROP(handle.0);
        let count = DragQueryFileW(drop, u32::MAX, None);
        let mut files = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let len = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
            files.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
        }
        Ok(Some(files))
    }
}

/// The formats of what's on the clipboard, to say what was copied when it
/// can't be read.
pub fn formats() -> Result<Vec<u32>> {
    unsafe {
        let _open = Open::new()?;
        let mut formats = Vec::new();
        let mut format = 0;
        loop {
            format = EnumClipboardFormats(format);
            if format == 0 {
                return Ok(formats);
            }
            formats.push(format);
        }
    }
}

/// Replaces whatever is on the clipboard with `text`.
pub fn write_text(text: &str) -> Result<()> {
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _open = Open::new()?;
        EmptyClipboard()?;
        let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2)?;
        let ptr = GlobalLock(memory) as *mut u16;
        if ptr.is_null() {
            _ = GlobalFree(Some(memory));
            bail!("couldn't lock clipboard memory");
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), ptr, units.len());
        _ = GlobalUnlock(memory);
        // The clipboard owns the memory once this succeeds.
        if let Err(e) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(memory.0))) {
            _ = GlobalFree(Some(memory));
            return Err(e.into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "uses the real clipboard"]
    fn round_trip() {
        let before = sequence();
        write_text("Conductor clipboard test ✓").unwrap();
        assert_ne!(sequence(), before);
        assert_eq!(read_text(1 << 20).unwrap().as_deref(), Some("Conductor clipboard test ✓"));
        assert_eq!(read_text(4).unwrap(), None);
    }
}
