//! GPU color conversion and scaling (BGRA desktop to NV12 for the encoder)
//! using the D3D11 video processor, so frames never leave the GPU.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{RECT, TRUE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

/// Encoders may still be reading earlier frames, so outputs rotate.
const RING: usize = 6;

pub struct Converter {
    vctx: ID3D11VideoContext,
    vp: ID3D11VideoProcessor,
    input: ID3D11VideoProcessorInputView,
    outputs: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
    next: usize,
    pub width: u32,
    pub height: u32,
}

impl Converter {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        source: &ID3D11Texture2D,
        (in_w, in_h): (u32, u32),
        (out_w, out_h): (u32, u32),
    ) -> Result<Self> {
        let vdev: ID3D11VideoDevice = device.cast().context("no video device")?;
        let vctx: ID3D11VideoContext = context.cast()?;
        let rate = DXGI_RATIONAL { Numerator: 60, Denominator: 1 };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: in_w,
            InputHeight: in_h,
            OutputFrameRate: rate,
            OutputWidth: out_w,
            OutputHeight: out_h,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumr = unsafe { vdev.CreateVideoProcessorEnumerator(&content)? };
        let vp = unsafe { vdev.CreateVideoProcessor(&enumr, 0)? };

        let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut input = None;
        unsafe { vdev.CreateVideoProcessorInputView(source, &enumr, &in_desc, Some(&mut input))? };
        let input = input.context("input view")?;

        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut outputs = Vec::with_capacity(RING);
        for _ in 0..RING {
            let tex = create_nv12(device, out_w, out_h)?;
            let mut view = None;
            unsafe { vdev.CreateVideoProcessorOutputView(&tex, &enumr, &out_desc, Some(&mut view))? };
            outputs.push((tex, view.context("output view")?));
        }

        unsafe {
            if let Ok(vctx1) = vctx.cast::<ID3D11VideoContext1>() {
                vctx1.VideoProcessorSetStreamColorSpace1(&vp, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                vctx1.VideoProcessorSetOutputColorSpace1(&vp, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            }
            vctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
            let src = RECT { left: 0, top: 0, right: in_w as i32, bottom: in_h as i32 };
            let dst = RECT { left: 0, top: 0, right: out_w as i32, bottom: out_h as i32 };
            vctx.VideoProcessorSetStreamSourceRect(&vp, 0, true, Some(&src));
            vctx.VideoProcessorSetStreamDestRect(&vp, 0, true, Some(&dst));
            vctx.VideoProcessorSetOutputTargetRect(&vp, true, Some(&dst));
        }

        Ok(Self { vctx, vp, input, outputs, next: 0, width: out_w, height: out_h })
    }

    /// Converts the source texture into the next NV12 texture and returns it.
    pub fn convert(&mut self) -> Result<ID3D11Texture2D> {
        let (tex, view) = &self.outputs[self.next];
        self.next = (self.next + 1) % RING;
        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: TRUE,
            pInputSurface: ManuallyDrop::new(Some(self.input.clone())),
            ..Default::default()
        };
        let mut streams = [stream];
        let res = unsafe { self.vctx.VideoProcessorBlt(&self.vp, view, 0, &streams) };
        unsafe { ManuallyDrop::drop(&mut streams[0].pInputSurface) };
        res.context("VideoProcessorBlt")?;
        Ok(tex.clone())
    }
}

fn create_nv12(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let mut desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_VIDEO_ENCODER.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    if unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.is_err() {
        desc.BindFlags = D3D11_BIND_RENDER_TARGET.0 as u32;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    }
    tex.context("CreateTexture2D NV12")
}

/// Largest even size that fits `max` while keeping the source aspect ratio.
pub fn fit(src: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = (src.0 as f64, src.1 as f64);
    let scale = (max.0 as f64 / sw).min(max.1 as f64 / sh).min(1.0);
    let even = |v: f64| ((v.round() as u32) & !1).max(2);
    (even(sw * scale), even(sh * scale))
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fit_keeps_aspect_and_even() {
        assert_eq!(fit((1920, 1080), (1920, 1080)), (1920, 1080));
        assert_eq!(fit((3840, 2160), (1920, 1080)), (1920, 1080));
        assert_eq!(fit((2560, 1600), (1280, 720)), (1152, 720));
        assert_eq!(fit((1366, 768), (4000, 4000)), (1366, 768));
        assert_eq!(fit((1365, 767), (4000, 4000)), (1364, 766));
    }
}
