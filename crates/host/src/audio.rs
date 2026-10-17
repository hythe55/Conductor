//! Sound playing on this PC, captured as the speakers get it and encoded as
//! Opus on a dedicated thread.
//!
//! Runs while the client wants sound. Nothing is sent while nothing plays, so
//! an idle session stays quiet on the network too.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eConsole,
    eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Opus runs at 48 kHz. Frames are 10 ms, short for low delay.
pub const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const FRAME: usize = 480;
const BITRATE: i32 = 128_000;
/// Silent frames still sent after the sound stops, so it ends cleanly.
const TRAILING_SILENCE: u32 = 5;
/// A pause in sending longer than this restarts the timestamps from the clock.
const GAP: Duration = Duration::from_millis(50);
/// How much sound Windows holds for us between reads.
const BUFFER: Duration = Duration::from_millis(100);
/// How often to check whether sound now goes to another device.
const DEVICE_CHECK: Duration = Duration::from_secs(2);
/// After capture fails, e.g. with no sound device, try again after this long.
const RETRY: Duration = Duration::from_secs(2);
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

pub struct Packet {
    pub data: Vec<u8>,
    /// Samples since the capture started, for the RTP timestamp.
    pub time: u64,
}

pub struct Audio {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Audio {
    pub fn start(out: mpsc::Sender<Packet>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                unsafe { _ = CoInitializeEx(None, COINIT_MULTITHREADED) };
                let mut encoder = match Encoder::new(out) {
                    Ok(e) => e,
                    Err(e) => return tracing::error!("audio: {e:#}"),
                };
                let mut failing = false;
                while !stop2.load(Ordering::Relaxed) {
                    match capture(&mut encoder, &stop2) {
                        Ok(()) => failing = false,
                        Err(e) => {
                            // Log once, not every retry while there's no sound device.
                            if !failing {
                                tracing::warn!("audio: {e:#}");
                            }
                            failing = true;
                            let until = Instant::now() + RETRY;
                            while Instant::now() < until && !stop2.load(Ordering::Relaxed) {
                                std::thread::sleep(Duration::from_millis(50));
                            }
                        }
                    }
                }
            })
            .expect("spawn audio thread");
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            _ = t.join();
        }
    }
}

/// Captures from the default output device until it changes or `stop` is set.
fn capture(encoder: &mut Encoder, stop: &AtomicBool) -> Result<()> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = devices.GetDefaultAudioEndpoint(eRender, eConsole).context("no sound output device")?;
        let id = device_id(&device)?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        // Windows converts whatever the device plays to this.
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: RATE,
            nAvgBytesPerSec: RATE * CHANNELS as u32 * 4,
            nBlockAlign: CHANNELS as u16 * 4,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                BUFFER.as_nanos() as i64 / 100,
                0,
                &format,
                None,
            )
            .context("couldn't capture sound")?;
        let event = Event(CreateEventW(None, false, false, None)?);
        client.SetEventHandle(event.0)?;
        let reader: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        tracing::debug!("capturing sound");

        let mut pcm: Vec<f32> = Vec::with_capacity(FRAME * CHANNELS * 8);
        let mut checked = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            // Loopback only signals while something plays, so wake up anyway to see `stop`.
            WaitForSingleObject(event.0, 20);
            while reader.GetNextPacketSize()? > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                reader.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let samples = frames as usize * CHANNELS;
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    pcm.resize(pcm.len() + samples, 0.0);
                } else {
                    pcm.extend_from_slice(std::slice::from_raw_parts(data as *const f32, samples));
                }
                reader.ReleaseBuffer(frames)?;
            }
            let whole = pcm.len() / (FRAME * CHANNELS) * FRAME * CHANNELS;
            for frame in pcm[..whole].chunks_exact(FRAME * CHANNELS) {
                encoder.encode(frame)?;
            }
            pcm.drain(..whole);
            if checked.elapsed() > DEVICE_CHECK {
                checked = Instant::now();
                let now = devices.GetDefaultAudioEndpoint(eRender, eConsole).ok();
                if now.and_then(|d| device_id(&d).ok()).as_ref() != Some(&id) {
                    tracing::debug!("sound output device changed");
                    break;
                }
            }
        }
        _ = client.Stop();
        Ok(())
    }
}

/// Opus frames timed for RTP, skipping silence.
struct Encoder {
    opus: opus::Encoder,
    out: mpsc::Sender<Packet>,
    started: Instant,
    /// Timestamp of the next packet.
    next: u64,
    last_sent: Option<Instant>,
    /// Silent frames in a row.
    silent: u32,
    buf: Vec<u8>,
}

impl Encoder {
    fn new(out: mpsc::Sender<Packet>) -> Result<Self> {
        let mut opus = opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::LowDelay)?;
        opus.set_bitrate(opus::Bitrate::Bits(BITRATE))?;
        Ok(Self { opus, out, started: Instant::now(), next: 0, last_sent: None, silent: 0, buf: vec![0; 1500] })
    }

    fn encode(&mut self, frame: &[f32]) -> Result<()> {
        if frame.iter().all(|&s| s == 0.0) {
            self.silent += 1;
            if self.silent > TRAILING_SILENCE {
                return Ok(());
            }
        } else {
            self.silent = 0;
        }
        // After a pause, start from the clock so the client plays it now
        // rather than queueing it behind the silence.
        if self.last_sent.is_none_or(|t| t.elapsed() > GAP) {
            let clock = self.started.elapsed().as_micros() as u64 * RATE as u64 / 1_000_000;
            self.next = self.next.max(clock);
        }
        let n = self.opus.encode_float(frame, &mut self.buf)?;
        // A session that can't keep up loses sound rather than holding up capture.
        _ = self.out.try_send(Packet { data: self.buf[..n].to_vec(), time: self.next });
        self.next += FRAME as u64;
        self.last_sent = Some(Instant::now());
        Ok(())
    }
}

unsafe fn device_id(device: &IMMDevice) -> Result<String> {
    unsafe {
        let id = device.GetId()?;
        let s = id.to_string();
        CoTaskMemFree(Some(id.0 as _));
        Ok(s?)
    }
}

struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        unsafe { _ = CloseHandle(self.0) };
    }
}
