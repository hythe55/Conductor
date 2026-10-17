//! Capture, convert and encode one display on a dedicated thread.
//!
//! Runs only while a client is connected. A static screen produces no frames
//! after a short refinement burst, so an idle session sends almost nothing.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc as tmpsc;

use crate::capture::{Capture, Frame, ResolutionChanged};
use crate::convert::{Converter, fit};
use crate::encoder::{EncodedFrame, Encoder, Profile};

/// Frames re-sent after motion stops so the encoder can sharpen the image.
const REFINE_FRAMES: u32 = 4;
const REFINE_INTERVAL: Duration = Duration::from_millis(50);

pub enum Control {
    Keyframe,
    Bitrate(u32),
}

pub struct Config {
    pub display: Option<String>,
    pub max_size: (u32, u32),
    pub fps: u32,
    pub bitrate: u32,
    pub profile: Profile,
}

pub enum PipelineEvent {
    Frame(EncodedFrame),
    Started { display: String, width: u32, height: u32, encoder: String, origin: (i32, i32), size: (u32, u32) },
    Failed(String),
}

pub struct Pipeline {
    control: mpsc::Sender<Control>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Pipeline {
    pub fn start(config: Config, out: tmpsc::Sender<PipelineEvent>) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("pipeline".into())
            .spawn(move || {
                if let Err(e) = run(config, rx, &out, &stop2) {
                    tracing::error!("pipeline: {e:#}");
                    _ = out.blocking_send(PipelineEvent::Failed(format!("{e:#}")));
                }
            })
            .expect("spawn pipeline thread");
        Self { control: tx, stop, thread: Some(thread) }
    }

    pub fn send(&self, c: Control) {
        _ = self.control.send(c);
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            _ = t.join();
        }
    }
}

fn run(
    mut config: Config,
    control: mpsc::Receiver<Control>,
    out: &tmpsc::Sender<PipelineEvent>,
    stop: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    unsafe {
        _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    crate::encoder::startup()?;
    let start = Instant::now();

    // Rebuilt from scratch when the display mode changes.
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let mut cap = Capture::new(config.display.as_deref())?;
        let size = fit((cap.width, cap.height), config.max_size);
        let mut conv = Converter::new(&cap.device, &cap.context, &cap.texture, (cap.width, cap.height), size)?;
        let mut enc = Encoder::new(&cap.device, cap.luid, size.0, size.1, config.fps, config.bitrate, config.profile)?;
        _ = out.blocking_send(PipelineEvent::Started {
            display: cap.id.clone(),
            width: size.0,
            height: size.1,
            encoder: enc.name.clone(),
            origin: cap.origin,
            size: (cap.width, cap.height),
        });

        let frame_interval = Duration::from_secs_f64(1.0 / config.fps as f64);
        let mut want_key = true;
        let mut have_image = false;
        let mut last_submit = Instant::now() - frame_interval;
        let mut refine_left = 0u32;
        let mut dirty = false;

        let result: Result<()> = (|| {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                while let Ok(c) = control.try_recv() {
                    match c {
                        Control::Keyframe => want_key = true,
                        Control::Bitrate(b) => {
                            config.bitrate = b;
                            enc.set_bitrate(b);
                        }
                    }
                }
                for f in enc.poll()? {
                    if out.try_send(PipelineEvent::Frame(f)).is_err() {
                        // Network side is backed up: drop and resync with a keyframe.
                        want_key = true;
                    }
                }
                if !enc.ready() {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }

                let wait = frame_interval.saturating_sub(last_submit.elapsed());
                let timeout = if refine_left > 0 || (want_key && have_image) {
                    wait.min(REFINE_INTERVAL)
                } else {
                    Duration::from_millis(100)
                };
                match cap.next(timeout.as_millis() as u32)? {
                    Frame::New => {
                        have_image = true;
                        dirty = true;
                        refine_left = REFINE_FRAMES;
                    }
                    Frame::Unchanged => {}
                }
                if last_submit.elapsed() < frame_interval {
                    continue;
                }
                let refine_due = refine_left > 0 && last_submit.elapsed() >= REFINE_INTERVAL;
                if !have_image || !(dirty || (want_key) || refine_due) {
                    continue;
                }
                if !dirty && refine_due {
                    refine_left -= 1;
                }
                let tex = conv.convert()?;
                let time = start.elapsed().as_nanos() as i64 / 100;
                for f in enc.encode(&tex, time, want_key)? {
                    if out.try_send(PipelineEvent::Frame(f)).is_err() {
                        want_key = true;
                    }
                }
                want_key = false;
                dirty = false;
                last_submit = Instant::now();
            }
            Ok(())
        })();

        match result {
            Ok(()) => break,
            Err(e) if e.is::<ResolutionChanged>() || e.root_cause().is::<ResolutionChanged>() => {
                tracing::info!("display mode changed, restarting pipeline");
                drop(enc);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
