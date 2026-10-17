//! A small picture of the main display, shown next to this PC in the device
//! list on the owner's other devices.
//!
//! Pictures are taken on one thread of their own, which follows the input
//! desktop so the lock and sign-in screens show too, and are kept for a few
//! seconds so several devices looking at once share one.

use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::sync::oneshot;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GetDC, ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR, GUID_WICPixelFormat32bppBGR,
    IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant,
    WICBitmapPaletteTypeCustom,
};
use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, STATFLAG_NONAME, STREAM_SEEK_SET,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::w;

/// Largest picture sent, in pixels. Rows show it at about a third of this.
const MAX_SIZE: (u32, u32) = (320, 200);
const QUALITY: f32 = 0.75;
/// A picture this recent is sent again rather than taking a new one.
const REUSE_FOR: Duration = Duration::from_secs(5);

type Reply = oneshot::Sender<Result<Vec<u8>>>;

static LAST: Mutex<Option<(Instant, Vec<u8>)>> = Mutex::new(None);
static THREAD: Mutex<Option<mpsc::Sender<Reply>>> = Mutex::new(None);

/// A JPEG of the main display as it looks now.
pub async fn take() -> Result<Vec<u8>> {
    if let Some((at, jpeg)) = LAST.lock().unwrap().as_ref() {
        if at.elapsed() < REUSE_FOR {
            return Ok(jpeg.clone());
        }
    }
    let (tx, rx) = oneshot::channel();
    {
        let mut thread = THREAD.lock().unwrap();
        let sender = thread.get_or_insert_with(start);
        if let Err(mpsc::SendError(tx)) = sender.send(tx) {
            // The thread is gone; start another.
            let sender = thread.insert(start());
            sender.send(tx).ok().context("the preview thread didn't start")?;
        }
    }
    let jpeg = rx.await.context("the preview thread stopped")??;
    *LAST.lock().unwrap() = Some((Instant::now(), jpeg.clone()));
    Ok(jpeg)
}

fn start() -> mpsc::Sender<Reply> {
    let (tx, rx) = mpsc::channel::<Reply>();
    let spawned = std::thread::Builder::new().name("preview".into()).spawn(move || {
        unsafe { _ = CoInitializeEx(None, COINIT_MULTITHREADED) };
        while let Ok(reply) = rx.recv() {
            // Requests that queued up while one was being taken share it.
            let result = shoot();
            let mut waiting = vec![reply];
            waiting.extend(rx.try_iter());
            for reply in waiting {
                _ = reply.send(result.as_ref().map(|j| j.clone()).map_err(|e| anyhow::anyhow!("{e:#}")));
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("starting the preview thread: {e}");
    }
    tx
}

fn shoot() -> Result<Vec<u8>> {
    crate::desktop::follow_input();
    let d = crate::capture::displays()?.into_iter().next().context("no display attached")?;
    let pixels = grab(d.x, d.y, d.width, d.height)?;
    encode(&pixels, d.width, d.height)
}

/// The screen's pixels in the given rectangle, as 32-bit BGR rows from the top.
fn grab(x: i32, y: i32, width: u32, height: u32) -> Result<Vec<u8>> {
    let (w, h) = (width as i32, height as i32);
    unsafe {
        let screen = GetDC(None);
        if screen.is_invalid() {
            bail!("couldn't open the screen");
        }
        let memory = CreateCompatibleDC(Some(screen));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // Negative for rows from the top.
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let result = match CreateDIBSection(Some(screen), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) => {
                let old = SelectObject(memory, bitmap.into());
                // CAPTUREBLT includes layered windows, such as the "being controlled" banner.
                let copied = BitBlt(memory, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT);
                let pixels = copied
                    .map(|()| std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize).to_vec());
                SelectObject(memory, old);
                _ = DeleteObject(bitmap.into());
                pixels.context("copying the screen")
            }
            Err(e) => Err(e).context("CreateDIBSection"),
        };
        _ = DeleteDC(memory);
        ReleaseDC(None, screen);
        result
    }
}

/// Size of the picture: `width` × `height` scaled down to fit [`MAX_SIZE`].
fn fit(width: u32, height: u32) -> (u32, u32) {
    let scale = (MAX_SIZE.0 as f64 / width as f64).min(MAX_SIZE.1 as f64 / height as f64).min(1.0);
    (((width as f64 * scale).round() as u32).max(1), ((height as f64 * scale).round() as u32).max(1))
}

fn encode(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let (w, h) = fit(width, height);
    unsafe {
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let source = wic.CreateBitmapFromMemory(width, height, &GUID_WICPixelFormat32bppBGR, width * 4, pixels)?;
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&source, w, h, WICBitmapInterpolationModeFant)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat24bppBGR,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;

        let stream = SHCreateMemStream(None).context("SHCreateMemStream")?;
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        let mut options: Option<IPropertyBag2> = None;
        encoder.CreateNewFrame(&mut frame, &mut options)?;
        let frame = frame.context("no JPEG frame")?;
        if let Some(options) = &options {
            let name = PROPBAG2 { pstrName: windows::core::PWSTR(w!("ImageQuality").as_ptr() as *mut _), ..Default::default() };
            _ = options.Write(1, &name, &VARIANT::from(QUALITY));
        }
        frame.Initialize(options.as_ref())?;
        frame.SetSize(w, h)?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut format)?;
        frame.WriteSource(&converter, std::ptr::null())?;
        frame.Commit()?;
        encoder.Commit()?;

        let mut stat = Default::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut jpeg = vec![0u8; stat.cbSize as usize];
        let mut read = 0;
        stream.Read(jpeg.as_mut_ptr().cast(), jpeg.len() as u32, Some(&mut read)).ok()?;
        jpeg.truncate(read as usize);
        Ok(jpeg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(fit(2560, 1600), (320, 200));
        assert_eq!(fit(1920, 1080), (320, 180));
        assert_eq!(fit(1080, 1920), (113, 200));
        assert_eq!(fit(200, 100), (200, 100));
    }

    #[test]
    fn encodes_jpeg() {
        unsafe { _ = CoInitializeEx(None, COINIT_MULTITHREADED) };
        let pixels = vec![128u8; 640 * 400 * 4];
        let jpeg = encode(&pixels, 640, 400).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }
}
