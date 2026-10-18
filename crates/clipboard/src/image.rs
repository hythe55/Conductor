//! Converting between the PNG files that pictures travel as and the bitmaps
//! (DIBs) most Windows apps copy and paste.

use anyhow::{Context, Result, bail};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR, GUID_WICPixelFormat32bppBGRA,
    IWICBitmapSource, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag2;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, STATFLAG_NONAME, STREAM_SEEK_SET,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::core::Interface;

/// Pictures bigger than this many pixels aren't converted.
const MAX_PIXELS: u64 = 120_000_000;

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

fn factory() -> Result<IWICImagingFactory> {
    unsafe {
        // Already set up on this thread is fine too.
        _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        Ok(CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?)
    }
}

/// A PNG of a clipboard DIB: a bitmap header, maybe colour masks or a
/// palette, then the pixels. Alpha in a DIB is rarely meant, so it's dropped.
pub fn dib_to_png(dib: &[u8]) -> Result<Vec<u8>> {
    let u32_at = |at: usize| dib.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let header = u32_at(0).context("the bitmap is empty")? as usize;
    let bits = dib.get(14..16).map(|b| u16::from_le_bytes([b[0], b[1]])).context("the bitmap is cut short")?;
    let compression = u32_at(16).context("the bitmap is cut short")?;
    let colors = u32_at(32).context("the bitmap is cut short")? as usize;
    // Colour masks follow a plain header; newer headers hold them.
    let masks = match (header, compression) {
        (40, BI_BITFIELDS) => 12,
        (40, BI_ALPHABITFIELDS) => 16,
        _ => 0,
    };
    let palette = 4 * if colors == 0 && bits <= 8 { 1 << bits } else { colors };
    // As a .bmp file, which WIC reads with all the variations bitmaps come in.
    let pixels_at = 14 + header + masks + palette;
    if pixels_at > 14 + dib.len() {
        bail!("the bitmap is cut short");
    }
    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&(pixels_at as u32).to_le_bytes());
    bmp.extend_from_slice(dib);
    unsafe {
        let wic = factory()?;
        let frame = decode(&wic, &bmp)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat24bppBGR,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        encode_png(&wic, &converter.cast()?)
    }
}

/// A bottom-up 24-bit DIB of a PNG, see-through parts shown over white, for
/// apps that paste bitmaps rather than PNGs.
pub fn png_to_dib(png: &[u8]) -> Result<Vec<u8>> {
    let (width, height, mut pixels) = unsafe {
        let wic = factory()?;
        let frame = decode(&wic, png)?;
        let (mut width, mut height) = (0, 0);
        frame.GetSize(&mut width, &mut height)?;
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            bail!("the picture is too big ({width}×{height})");
        }
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        converter.CopyPixels(std::ptr::null(), width * 4, &mut pixels)?;
        (width as usize, height as usize, pixels)
    };
    for p in pixels.as_chunks_mut::<4>().0 {
        let a = u32::from(p[3]);
        for c in &mut p[..3] {
            *c = ((u32::from(*c) * a + 255 * (255 - a) + 127) / 255) as u8;
        }
    }
    let stride = (width * 3 + 3) & !3;
    let mut dib = Vec::with_capacity(40 + stride * height);
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(width as i32).to_le_bytes());
    // Positive: the bottom row comes first, which every app reads.
    dib.extend_from_slice(&(height as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&24u16.to_le_bytes());
    dib.extend_from_slice(&BI_RGB.to_le_bytes());
    dib.extend_from_slice(&((stride * height) as u32).to_le_bytes());
    // Resolution and colour counts, unused.
    dib.extend_from_slice(&[0; 16]);
    for row in pixels.chunks_exact(width * 4).rev() {
        let start = dib.len();
        for p in row.as_chunks::<4>().0 {
            dib.extend_from_slice(&p[..3]);
        }
        dib.resize(start + stride, 0);
    }
    Ok(dib)
}

unsafe fn decode(wic: &IWICImagingFactory, data: &[u8]) -> Result<IWICBitmapSource> {
    unsafe {
        let stream = wic.CreateStream()?;
        stream.InitializeFromMemory(data)?;
        let decoder = wic
            .CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .context("reading the picture")?;
        Ok(decoder.GetFrame(0)?.cast()?)
    }
}

unsafe fn encode_png(wic: &IWICImagingFactory, source: &IWICBitmapSource) -> Result<Vec<u8>> {
    unsafe {
        let (mut width, mut height) = (0, 0);
        source.GetSize(&mut width, &mut height)?;
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            bail!("the picture is too big ({width}×{height})");
        }
        let stream = SHCreateMemStream(None).context("SHCreateMemStream")?;
        let encoder = wic.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        let mut options: Option<IPropertyBag2> = None;
        encoder.CreateNewFrame(&mut frame, &mut options)?;
        let frame = frame.context("no PNG frame")?;
        frame.Initialize(options.as_ref())?;
        frame.SetSize(width, height)?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        frame.SetPixelFormat(&mut format)?;
        frame.WriteSource(source, std::ptr::null())?;
        frame.Commit()?;
        encoder.Commit()?;

        let mut stat = Default::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut png = vec![0u8; stat.cbSize as usize];
        let mut read = 0;
        stream.Read(png.as_mut_ptr().cast(), png.len() as u32, Some(&mut read)).ok()?;
        png.truncate(read as usize);
        Ok(png)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        // A 3×2 bitmap, rows padded to 12 bytes, bottom row first.
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&3i32.to_le_bytes());
        dib.extend_from_slice(&2i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        let bottom = [255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0];
        let top = [10, 20, 30, 40, 50, 60, 70, 80, 90, 0, 0, 0];
        dib.extend_from_slice(&bottom);
        dib.extend_from_slice(&top);
        let png = dib_to_png(&dib).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let back = png_to_dib(&png).unwrap();
        assert_eq!(&back[4..12], &dib[4..12]);
        assert_eq!(&back[40..], &dib[40..]);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(dib_to_png(&[]).is_err());
        assert!(dib_to_png(&[40, 0, 0, 0, 1]).is_err());
        assert!(png_to_dib(b"not a picture").is_err());
    }
}
