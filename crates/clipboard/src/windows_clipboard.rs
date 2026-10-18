use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIB, CF_HDROP, CF_UNICODETEXT};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::core::{PCWSTR, w};

use crate::image::{dib_to_png, png_to_dib};

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
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    let _open = Open::new()?;
    unsafe { EmptyClipboard()? };
    set(u32::from(CF_UNICODETEXT.0), &bytes)
}

/// The picture on the clipboard as a PNG, or None when it holds something
/// else or a PNG bigger than `max_bytes`.
pub fn read_image(max_bytes: usize) -> Result<Option<Vec<u8>>> {
    // Apps that copy pictures with see-through parts put a PNG there too; the
    // rest only a bitmap, which is converted once the clipboard is closed.
    let png = registered(w!("PNG"));
    let dib = u32::from(CF_DIB.0);
    let available = |f: u32| f != 0 && unsafe { IsClipboardFormatAvailable(f) }.is_ok();
    let (format, is_png) = if available(png) {
        (png, true)
    } else if available(dib) {
        (dib, false)
    } else {
        return Ok(None);
    };
    let data = {
        let _open = Open::new()?;
        // A bitmap gets smaller as a PNG, usually a lot.
        let Some(data) = get(format, if is_png { max_bytes } else { max_bytes.saturating_mul(8) }) else {
            return Ok(None);
        };
        data
    };
    if is_png {
        return Ok(Some(data));
    }
    let png = dib_to_png(&data)?;
    Ok((png.len() <= max_bytes).then_some(png))
}

/// Replaces whatever is on the clipboard with a picture, from a PNG: as the
/// PNG for apps that take one, and as a bitmap for the rest.
pub fn write_image(png: &[u8]) -> Result<()> {
    let dib = png_to_dib(png)?;
    let _open = Open::new()?;
    unsafe { EmptyClipboard()? };
    set(registered(w!("PNG")), png)?;
    set(u32::from(CF_DIB.0), &dib)
}

/// Replaces whatever is on the clipboard with files, as if they'd been copied
/// in Explorer.
pub fn write_files(paths: &[PathBuf]) -> Result<()> {
    // A DROPFILES header, saying the names that follow are UTF-16, then each
    // name ending in a 0, then another 0.
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(&20u32.to_le_bytes());
    data.extend_from_slice(&[0; 12]);
    data.extend_from_slice(&1u32.to_le_bytes());
    for path in paths {
        for unit in Path::new(path).as_os_str().encode_wide().chain([0]) {
            data.extend_from_slice(&unit.to_le_bytes());
        }
    }
    data.extend_from_slice(&[0, 0]);
    let _open = Open::new()?;
    unsafe { EmptyClipboard()? };
    set(u32::from(CF_HDROP.0), &data)?;
    // Pasting copies them rather than moving them.
    const DROPEFFECT_COPY: u32 = 1;
    set(registered(w!("Preferred DropEffect")), &DROPEFFECT_COPY.to_le_bytes())
}

fn registered(name: PCWSTR) -> u32 {
    unsafe { RegisterClipboardFormatW(name) }
}

/// A copy of one format on the open clipboard, unless it's bigger than `max_bytes`.
fn get(format: u32, max_bytes: usize) -> Option<Vec<u8>> {
    unsafe {
        let memory = HGLOBAL(GetClipboardData(format).ok()?.0);
        let size = GlobalSize(memory);
        if size == 0 || size > max_bytes {
            return None;
        }
        let ptr = GlobalLock(memory) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let data = std::slice::from_raw_parts(ptr, size).to_vec();
        _ = GlobalUnlock(memory);
        Some(data)
    }
}

/// Adds one format to the open, emptied clipboard.
fn set(format: u32, data: &[u8]) -> Result<()> {
    if format == 0 {
        bail!("couldn't register a clipboard format");
    }
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1))?;
        let ptr = GlobalLock(memory) as *mut u8;
        if ptr.is_null() {
            _ = GlobalFree(Some(memory));
            bail!("couldn't lock clipboard memory");
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        _ = GlobalUnlock(memory);
        // The clipboard owns the memory once this succeeds.
        if let Err(e) = SetClipboardData(format, Some(HANDLE(memory.0))) {
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

    #[test]
    #[ignore = "uses the real clipboard"]
    fn pictures_and_files() {
        // A 2×1 picture, a red pixel and a half see-through one.
        let dib = {
            let mut d = Vec::new();
            d.extend_from_slice(&40u32.to_le_bytes());
            d.extend_from_slice(&2i32.to_le_bytes());
            d.extend_from_slice(&1i32.to_le_bytes());
            d.extend_from_slice(&1u16.to_le_bytes());
            d.extend_from_slice(&32u16.to_le_bytes());
            d.extend_from_slice(&[0; 24]);
            d.extend_from_slice(&[0, 0, 255, 255, 0, 0, 0, 128]);
            d
        };
        let png = dib_to_png(&dib).unwrap();
        write_image(&png).unwrap();
        assert_eq!(read_image(1 << 20).unwrap().as_deref(), Some(png.as_slice()));
        assert_eq!(read_text(100).unwrap(), None);

        let files = vec![PathBuf::from(r"C:\Windows\win.ini"), PathBuf::from(r"C:\Windows\system.ini")];
        write_files(&files).unwrap();
        assert_eq!(read_files().unwrap(), Some(files));
        assert_eq!(read_image(1 << 20).unwrap(), None);
    }
}
