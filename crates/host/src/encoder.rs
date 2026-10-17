//! H.264 encoding through Media Foundation.
//!
//! Prefers the GPU vendor's hardware encoder (NVENC, Quick Sync, AMF all ship
//! as MFTs) on the adapter that owns the captured display, and falls back to
//! Microsoft's software encoder. Tuned for latency: CBR, no B-frames, low
//! latency mode, keyframes only on request.

use std::mem::ManuallyDrop;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{LUID, VARIANT_TRUE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::*;
use windows::core::{GUID, Interface};

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    /// Sample time in 100ns units, as given to `encode`.
    pub time: i64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
    ConstrainedBaseline,
    High,
}

pub struct Encoder {
    transform: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    codec: Option<ICodecAPI>,
    _manager: IMFDXGIDeviceManager,
    need_input: u32,
    provides_samples: bool,
    output_buffer_size: u32,
    sequence_header: Option<Vec<u8>>,
    pub name: String,
    pub hardware: bool,
    pub width: u32,
    pub height: u32,
    fps: u32,
}

pub fn startup() -> Result<()> {
    unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup") }
}

fn variant_u32(v: u32) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = v;
    }
    var
}

fn variant_bool(v: bool) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = if v { VARIANT_TRUE } else { Default::default() };
    }
    var
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn friendly_name(activate: &IMFActivate) -> String {
    unsafe {
        let Ok(len) = activate.GetStringLength(&MFT_FRIENDLY_NAME_Attribute) else {
            return "unknown".into();
        };
        let mut buf = vec![0u16; len as usize + 1];
        if activate.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, None).is_err() {
            return "unknown".into();
        }
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

fn enum_encoders(luid: Option<LUID>) -> Result<Vec<IMFActivate>> {
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
    let mut ptr: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        match luid {
            Some(luid) => {
                let mut attrs = None;
                MFCreateAttributes(&mut attrs, 1)?;
                let attrs = attrs.context("attributes")?;
                let bytes = std::slice::from_raw_parts(&luid as *const LUID as *const u8, size_of::<LUID>());
                attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, bytes)?;
                MFTEnum2(
                    MFT_CATEGORY_VIDEO_ENCODER,
                    MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                    Some(&input),
                    Some(&output),
                    &attrs,
                    &mut ptr,
                    &mut count,
                )?;
            }
            None => MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut ptr,
                &mut count,
            )?,
        }
        let mut list = Vec::new();
        if !ptr.is_null() {
            for i in 0..count as usize {
                if let Some(a) = (*ptr.add(i)).take() {
                    list.push(a);
                }
            }
            CoTaskMemFree(Some(ptr as *const _));
        }
        Ok(list)
    }
}

impl Encoder {
    pub fn new(
        device: &ID3D11Device,
        luid: LUID,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        profile: Profile,
    ) -> Result<Self> {
        let mut last_err = None;
        for (hardware, candidates) in [(true, enum_encoders(Some(luid))), (false, enum_encoders(None))] {
            for activate in candidates.unwrap_or_default() {
                let name = friendly_name(&activate);
                match Self::open(&activate, device, width, height, fps, bitrate, profile) {
                    Ok(mut enc) => {
                        tracing::info!("encoder: {name} ({width}x{height}@{fps})");
                        enc.name = name;
                        enc.hardware = hardware;
                        return Ok(enc);
                    }
                    Err(e) => {
                        tracing::warn!("encoder {name} unusable: {e:#}");
                        unsafe { _ = activate.ShutdownObject() };
                        last_err = Some(e);
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no H.264 encoder found")))
    }

    fn open(
        activate: &IMFActivate,
        device: &ID3D11Device,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        profile: Profile,
    ) -> Result<Self> {
        unsafe {
            let transform: IMFTransform = activate.ActivateObject()?;
            let attrs = transform.GetAttributes().ok();
            let is_async = attrs
                .as_ref()
                .and_then(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).ok())
                .unwrap_or(0)
                != 0;
            if let Some(a) = &attrs {
                if is_async {
                    a.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
                }
                _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }

            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("device manager")?;
            manager.ResetDevice(device, token)?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("encoder does not accept D3D11 surfaces")?;

            let codec = transform.cast::<ICodecAPI>().ok();
            if let Some(c) = &codec {
                _ = c.SetValue(&CODECAPI_AVEncCommonRateControlMode, &variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32));
                _ = c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate));
                _ = c.SetValue(&CODECAPI_AVLowLatencyMode, &variant_bool(true));
                _ = c.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &variant_u32(0));
                _ = c.SetValue(&CODECAPI_AVEncMPVGOPSize, &variant_u32(fps * 60));
            }

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            let p = match profile {
                Profile::High => eAVEncH264VProfile_High,
                Profile::ConstrainedBaseline => eAVEncH264VProfile_ConstrainedBase,
            };
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, p.0 as u32)?;
            transform.SetOutputType(0, &out, 0).context("SetOutputType")?;

            let mut set_input = false;
            for i in 0.. {
                let Ok(t) = transform.GetInputAvailableType(0, i) else { break };
                if t.GetGUID(&MF_MT_SUBTYPE)? != MFVideoFormat_NV12 {
                    continue;
                }
                t.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
                t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
                transform.SetInputType(0, &t, 0).context("SetInputType")?;
                set_input = true;
                break;
            }
            if !set_input {
                bail!("encoder has no NV12 input");
            }

            let info = transform.GetOutputStreamInfo(0)?;
            let provides_samples = info.dwFlags
                & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32
                != 0;
            let sequence_header = transform
                .GetOutputCurrentType(0)
                .ok()
                .and_then(|t| blob(&t, &MF_MT_MPEG_SEQUENCE_HEADER));

            transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;

            let events = if is_async { Some(transform.cast::<IMFMediaEventGenerator>()?) } else { None };
            Ok(Self {
                transform,
                need_input: if is_async { 0 } else { 1 },
                events,
                codec,
                _manager: manager,
                provides_samples,
                output_buffer_size: info.cbSize.max(width * height * 3 / 2),
                sequence_header,
                name: String::new(),
                hardware: true,
                width,
                height,
                fps,
            })
        }
    }

    /// Handles pending encoder events and returns any finished frames.
    pub fn poll(&mut self) -> Result<Vec<EncodedFrame>> {
        let mut frames = Vec::new();
        let Some(events) = self.events.clone() else { return Ok(frames) };
        loop {
            let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(e) => e,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                Err(e) => return Err(e).context("encoder GetEvent"),
            };
            match MF_EVENT_TYPE(unsafe { event.GetType()? } as i32) {
                METransformNeedInput => self.need_input += 1,
                METransformHaveOutput => {
                    if let Some(f) = self.pull()? {
                        frames.push(f);
                    }
                }
                _ => {}
            }
        }
        Ok(frames)
    }

    pub fn ready(&self) -> bool {
        self.need_input > 0
    }

    pub fn encode(&mut self, texture: &ID3D11Texture2D, time: i64, keyframe: bool) -> Result<Vec<EncodedFrame>> {
        unsafe {
            if keyframe {
                if let Some(c) = &self.codec {
                    _ = c.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &variant_u32(1));
                }
            }
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, 0, false)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(10_000_000 / self.fps as i64)?;
            self.transform.ProcessInput(0, &sample, 0).context("ProcessInput")?;
        }
        self.need_input = self.need_input.saturating_sub(1);
        if self.events.is_some() {
            return Ok(Vec::new());
        }
        // Synchronous (software) encoder: drain right away.
        self.need_input = 1;
        let mut frames = Vec::new();
        while let Some(f) = self.pull()? {
            frames.push(f);
        }
        Ok(frames)
    }

    pub fn set_bitrate(&self, bitrate: u32) {
        if let Some(c) = &self.codec {
            unsafe { _ = c.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &variant_u32(bitrate)) };
        }
    }

    fn pull(&mut self) -> Result<Option<EncodedFrame>> {
        unsafe {
            let sample = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.output_buffer_size)?)?;
                Some(s)
            };
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let res = self.transform.ProcessOutput(0, &mut out, &mut status);
            let sample = ManuallyDrop::take(&mut out[0].pSample);
            drop(ManuallyDrop::take(&mut out[0].pEvents));
            match res {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    let t = self.transform.GetOutputAvailableType(0, 0)?;
                    self.transform.SetOutputType(0, &t, 0)?;
                    self.sequence_header = blob(&t, &MF_MT_MPEG_SEQUENCE_HEADER);
                    return Ok(None);
                }
                Err(e) => return Err(e).context("ProcessOutput"),
            }
            let Some(sample) = sample else { return Ok(None) };
            let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let time = sample.GetSampleTime().unwrap_or(0);
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let bytes = std::slice::from_raw_parts(ptr, len as usize);
            let mut data = Vec::with_capacity(len as usize + 64);
            if keyframe && !has_sps(bytes) {
                if let Some(h) = &self.sequence_header {
                    data.extend_from_slice(h);
                }
            }
            data.extend_from_slice(bytes);
            buffer.Unlock()?;
            Ok(Some(EncodedFrame { data, keyframe, time }))
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Some(ev) = self.events.as_ref().and_then(|e| e.cast::<IMFShutdown>().ok()) {
                _ = ev.Shutdown();
            }
        }
    }
}

fn blob(t: &IMFMediaType, key: &GUID) -> Option<Vec<u8>> {
    unsafe {
        let size = t.GetBlobSize(key).ok()?;
        let mut buf = vec![0u8; size as usize];
        t.GetBlob(key, &mut buf, None).ok()?;
        Some(buf)
    }
}

/// True if an Annex B stream contains a sequence parameter set.
fn has_sps(data: &[u8]) -> bool {
    data.windows(4).any(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && (w[3] & 0x1f) == 7)
}

#[cfg(test)]
mod tests {
    #[test]
    fn detects_sps() {
        assert!(super::has_sps(&[0, 0, 0, 1, 0x67, 0x64]));
        assert!(!super::has_sps(&[0, 0, 0, 1, 0x65, 0x88]));
    }
}
