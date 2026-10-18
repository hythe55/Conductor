//! Display enumeration and capture through DXGI Desktop Duplication.
//!
//! Desktop Duplication only hands out a frame when something on screen
//! changed, so an idle desktop costs nothing to capture. Everyone watching a
//! display shares its capture.

use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result, bail};
use conductor_proto::DisplayInfo;
use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

pub struct Output {
    pub adapter: IDXGIAdapter1,
    pub output: IDXGIOutput,
    pub desc: DXGI_OUTPUT_DESC,
    pub luid: LUID,
}

fn device_name(desc: &DXGI_OUTPUT_DESC) -> String {
    let len = desc.DeviceName.iter().position(|&c| c == 0).unwrap_or(desc.DeviceName.len());
    String::from_utf16_lossy(&desc.DeviceName[..len])
}

/// All outputs currently attached to the desktop.
pub fn outputs() -> Result<Vec<Output>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut found = Vec::new();
    let mut a = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(a) } {
        a += 1;
        let luid = unsafe { adapter.GetDesc1()? }.AdapterLuid;
        let mut o = 0;
        while let Ok(output) = unsafe { adapter.EnumOutputs(o) } {
            o += 1;
            let desc = unsafe { output.GetDesc()? };
            if desc.AttachedToDesktop.as_bool() {
                found.push(Output { adapter: adapter.clone(), output, desc, luid });
            }
        }
    }
    Ok(found)
}

pub fn displays() -> Result<Vec<DisplayInfo>> {
    let names = crate::displays::friendly_names();
    let mut list: Vec<DisplayInfo> = outputs()?
        .iter()
        .map(|o| {
            let r = o.desc.DesktopCoordinates;
            let id = device_name(&o.desc);
            let (name, is_virtual) = names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| (id.trim_start_matches(r"\\.\").to_string(), false));
            DisplayInfo {
                id,
                name,
                x: r.left,
                y: r.top,
                width: (r.right - r.left) as u32,
                height: (r.bottom - r.top) as u32,
                primary: r.left == 0 && r.top == 0,
                is_virtual,
            }
        })
        .collect();
    list.sort_by_key(|d| (!d.primary, d.x, d.y));
    Ok(list)
}

pub fn find_output(id: Option<&str>) -> Result<Output> {
    let mut all = outputs()?;
    let idx = match id {
        Some(id) => all.iter().position(|o| device_name(&o.desc) == id),
        None => all.iter().position(|o| {
            let r = o.desc.DesktopCoordinates;
            r.left == 0 && r.top == 0
        }),
    }
    .or(if all.is_empty() { None } else { Some(0) })
    .context("no display attached")?;
    Ok(all.swap_remove(idx))
}

pub fn create_device(adapter: &IDXGIAdapter1) -> Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device: ID3D11Device = device.context("no D3D11 device")?;
    let context = context.context("no D3D11 context")?;
    // Media Foundation encoders touch the device from their own threads.
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        unsafe { _ = mt.SetMultithreadProtected(true) };
    }
    Ok((device, context))
}

pub enum Frame {
    /// The desktop image changed; `Capture::texture` holds it.
    New,
    /// Nothing changed within the timeout.
    Unchanged,
}

/// Windows lets a program duplicate each display only once, so everyone
/// watching a display shares its duplication, latest image and device.
struct Shared {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    luid: LUID,
    width: u32,
    height: u32,
    origin: (i32, i32),
    output: IDXGIOutput,
    texture: ID3D11Texture2D,
    state: Mutex<State>,
}

struct State {
    dup: Option<IDXGIOutputDuplication>,
    /// Counts the images copied into the texture.
    images: u64,
    /// The display's mode changed, so whoever is using this starts over.
    changed: bool,
}

// The device is protected for use from several threads, and the duplication
// is only touched with `state` locked.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

/// Each display being captured, by name.
static SHARED: Mutex<Vec<(String, Weak<Shared>)>> = Mutex::new(Vec::new());

impl Shared {
    fn duplicate(&self) -> Result<IDXGIOutputDuplication> {
        let dup = match self.output.cast::<IDXGIOutput5>() {
            Ok(o5) => unsafe { o5.DuplicateOutput1(&self.device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) },
            Err(_) => unsafe { self.output.cast::<IDXGIOutput1>()?.DuplicateOutput(&self.device) },
        }
        .context("DuplicateOutput")?;
        let desc = unsafe { dup.GetDesc() };
        if desc.ModeDesc.Width != self.width || desc.ModeDesc.Height != self.height {
            bail!(ResolutionChanged);
        }
        Ok(dup)
    }
}

pub struct Capture {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub luid: LUID,
    pub width: u32,
    pub height: u32,
    pub id: String,
    pub origin: (i32, i32),
    /// The latest image, copied out of the duplication so its frame can be
    /// released right away and the image re-encoded later on request.
    pub texture: ID3D11Texture2D,
    shared: Arc<Shared>,
    /// How many images there had been when this last took one.
    seen: u64,
}

impl Capture {
    pub fn new(display: Option<&str>) -> Result<Self> {
        crate::desktop::follow_input();
        let out = find_output(display)?;
        let id = device_name(&out.desc);
        let r = out.desc.DesktopCoordinates;
        let (width, height) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
        let mut all = SHARED.lock().unwrap();
        all.retain(|(_, s)| s.strong_count() > 0);
        let current = all.iter().find(|(d, _)| *d == id).and_then(|(_, s)| s.upgrade());
        let (shared, seen) = match current {
            Some(s) if (s.width, s.height) == (width, height) && !s.state.lock().unwrap().changed => {
                // Starts with the image the others already have.
                let seen = s.state.lock().unwrap().images.saturating_sub(1);
                (s, seen)
            }
            old => {
                let (device, context) = create_device(&out.adapter)?;
                let texture = create_bgra(&device, width, height)?;
                let s = Shared {
                    device,
                    context,
                    luid: out.luid,
                    width,
                    height,
                    origin: (r.left, r.top),
                    output: out.output,
                    texture,
                    state: Mutex::new(State { dup: None, images: 0, changed: false }),
                };
                match s.duplicate() {
                    Ok(dup) => s.state.lock().unwrap().dup = Some(dup),
                    // One from before the display changed may still hold it for a moment.
                    Err(e) if old.is_some() && !e.is::<ResolutionChanged>() => {}
                    Err(e) => return Err(e),
                }
                let s = Arc::new(s);
                all.retain(|(d, _)| *d != id);
                all.push((id.clone(), Arc::downgrade(&s)));
                (s, 0)
            }
        };
        Ok(Self {
            device: shared.device.clone(),
            context: shared.context.clone(),
            luid: shared.luid,
            width,
            height,
            id,
            origin: shared.origin,
            texture: shared.texture.clone(),
            shared,
            seen,
        })
    }

    /// Waits up to `timeout_ms` for a new desktop image.
    pub fn next(&mut self, timeout_ms: u32) -> Result<Frame> {
        let mut state = self.shared.state.lock().unwrap();
        if state.changed {
            bail!(ResolutionChanged);
        }
        // Someone else watching took a newer image.
        if state.images > self.seen {
            self.seen = state.images;
            return Ok(Frame::New);
        }
        if state.dup.is_none() {
            // Lost earlier (mode change, secure desktop). Keep trying quietly.
            crate::desktop::follow_input();
            match self.shared.duplicate() {
                Ok(dup) => state.dup = Some(dup),
                Err(e) if e.is::<ResolutionChanged>() => {
                    state.changed = true;
                    return Err(e);
                }
                Err(_) => {
                    drop(state);
                    std::thread::sleep(std::time::Duration::from_millis(timeout_ms.max(50) as u64));
                    return Ok(Frame::Unchanged);
                }
            }
        }
        let dup = state.dup.as_ref().unwrap();
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        match unsafe { dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(Frame::Unchanged),
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST || e.code() == DXGI_ERROR_INVALID_CALL => {
                state.dup = None;
                return Ok(Frame::Unchanged);
            }
            Err(e) => return Err(e).context("AcquireNextFrame"),
        }
        let changed = info.LastPresentTime != 0;
        if changed {
            if let Some(res) = resource {
                let tex: ID3D11Texture2D = res.cast()?;
                unsafe { self.context.CopyResource(&self.texture, &tex) };
            }
        }
        unsafe { _ = dup.ReleaseFrame() };
        if !changed {
            return Ok(Frame::Unchanged);
        }
        state.images += 1;
        self.seen = state.images;
        Ok(Frame::New)
    }
}

#[derive(Debug)]
pub struct ResolutionChanged;

impl std::fmt::Display for ResolutionChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("display resolution changed")
    }
}

impl std::error::Error for ResolutionChanged {}

fn create_bgra(device: &ID3D11Device, width: u32, height: u32) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    tex.context("CreateTexture2D")
}
