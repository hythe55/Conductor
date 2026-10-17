//! Capture, convert and encode one display on a dedicated thread.
//!
//! Runs only while a client is connected. A static screen produces no frames
//! after a short refinement burst, so an idle session sends almost nothing.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc as tmpsc;
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;

use crate::capture::{Capture, Frame, ResolutionChanged};
use crate::convert::{Converter, fit};
use crate::encoder::{EncodedFrame, Encoder, Profile};

/// Frames re-sent after motion stops so the encoder can sharpen the image.
const REFINE_FRAMES: u32 = 4;
const REFINE_INTERVAL: Duration = Duration::from_millis(50);
const STATS_EVERY: Duration = Duration::from_secs(5);
/// An encoder that can't change bitrate on the fly is restarted when the target
/// falls below or rises above these fractions of what it runs at. Dropping is
/// urgent (the connection can't carry the stream); rising can wait to settle.
const RESTART_BELOW: f64 = 0.75;
const RESTART_ABOVE: f64 = 1.5;
const RESTART_DOWN_AFTER: Duration = Duration::from_secs(1);
const RESTART_UP_AFTER: Duration = Duration::from_secs(5);

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
        let mut enc_bitrate = config.bitrate;
        let mut last_restart = Instant::now();
        let mut opening: Option<Opening> = None;
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
        // Frames arrive on the display's refresh, give or take a little. Submitting on a
        // schedule with some slack keeps every frame of a matching refresh rate instead of
        // skipping one whenever a frame lands slightly early.
        let slack = frame_interval / 4;
        let mut next_due = Instant::now();
        let mut last_submit = Instant::now() - frame_interval;
        let mut refine_left = 0u32;
        let mut dirty = false;

        let mut stats = Stats::new();
        let result: Result<()> = (|| {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                stats.report();
                while let Ok(c) = control.try_recv() {
                    match c {
                        Control::Keyframe => want_key = true,
                        Control::Bitrate(b) => {
                            config.bitrate = b;
                            if enc.set_bitrate(b) {
                                enc_bitrate = b;
                            }
                        }
                    }
                }
                match opening.as_ref().map(|o| o.rx.try_recv()) {
                    None => {
                        if restart_due(enc_bitrate, config.bitrate, last_restart.elapsed()) {
                            opening = Some(Opening::start(&cap.device, cap.luid, size, &config));
                        }
                    }
                    Some(Err(mpsc::TryRecvError::Empty)) => {}
                    Some(result) => {
                        let o = opening.take().expect("checked above");
                        last_restart = Instant::now();
                        match result {
                            Ok(Ok(new)) => {
                                tracing::debug!(
                                    "restarted the encoder for {:.1} Mbps in {} ms",
                                    o.bitrate as f64 / 1e6,
                                    o.started.elapsed().as_millis()
                                );
                                enc = new;
                                enc_bitrate = o.bitrate;
                                want_key = true;
                            }
                            Ok(Err(e)) => tracing::warn!("couldn't restart the encoder: {e:#}"),
                            Err(_) => tracing::warn!("couldn't restart the encoder"),
                        }
                    }
                }
                for f in enc.poll()? {
                    stats.output(f.time);
                    if out.try_send(PipelineEvent::Frame(f)).is_err() {
                        // Network side is backed up: drop and resync with a keyframe.
                        stats.dropped += 1;
                        want_key = true;
                    }
                }
                if !enc.ready() {
                    let waited = Instant::now();
                    std::thread::sleep(Duration::from_millis(1));
                    stats.waiting += waited.elapsed();
                    continue;
                }

                let wait = next_due.saturating_duration_since(Instant::now() + slack);
                let timeout = if refine_left > 0 || (want_key && have_image) {
                    wait.min(REFINE_INTERVAL)
                } else {
                    Duration::from_millis(100)
                };
                match cap.next(timeout.as_millis() as u32)? {
                    Frame::New => {
                        stats.captured += 1;
                        have_image = true;
                        dirty = true;
                        refine_left = REFINE_FRAMES;
                    }
                    Frame::Unchanged => {}
                }
                if Instant::now() + slack < next_due {
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
                stats.submitted(time);
                for f in enc.encode(&tex, time, want_key)? {
                    stats.output(f.time);
                    if out.try_send(PipelineEvent::Frame(f)).is_err() {
                        stats.dropped += 1;
                        want_key = true;
                    }
                }
                want_key = false;
                dirty = false;
                last_submit = Instant::now();
                // After a pause, start a new schedule rather than bursting to catch up.
                next_due = (next_due + frame_interval).max(last_submit + frame_interval - slack);
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

/// A replacement encoder being opened on another thread. Opening one takes long
/// enough (half a second for Intel's) to show as a stall if the stream waited.
struct Opening {
    bitrate: u32,
    started: Instant,
    rx: mpsc::Receiver<Result<Encoder>>,
}

impl Opening {
    fn start(device: &ID3D11Device, luid: LUID, size: (u32, u32), config: &Config) -> Self {
        let (tx, rx) = mpsc::channel();
        let device = device.clone();
        let (fps, bitrate, profile) = (config.fps, config.bitrate, config.profile);
        std::thread::spawn(move || {
            unsafe {
                _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
            }
            _ = tx.send(Encoder::new(&device, luid, size.0, size.1, fps, bitrate, profile));
        });
        Self { bitrate, started: Instant::now(), rx }
    }
}

/// Whether an encoder running at `current` should be restarted to reach `wanted`.
fn restart_due(current: u32, wanted: u32, since_last: Duration) -> bool {
    let ratio = wanted as f64 / current as f64;
    (ratio < RESTART_BELOW && since_last >= RESTART_DOWN_AFTER) || (ratio > RESTART_ABOVE && since_last >= RESTART_UP_AFTER)
}

/// Counts what the pipeline did, logged every few seconds at debug level.
struct Stats {
    since: Instant,
    captured: u32,
    encoded: u32,
    sent: u32,
    dropped: u32,
    waiting: Duration,
    latency: Duration,
    latency_max: Duration,
    /// Frames handed to the encoder and not yet returned: (sample time, when).
    in_flight: VecDeque<(i64, Instant)>,
}

impl Stats {
    fn new() -> Self {
        Self {
            since: Instant::now(),
            captured: 0,
            encoded: 0,
            sent: 0,
            dropped: 0,
            waiting: Duration::ZERO,
            latency: Duration::ZERO,
            latency_max: Duration::ZERO,
            in_flight: VecDeque::new(),
        }
    }

    fn submitted(&mut self, time: i64) {
        self.encoded += 1;
        self.in_flight.push_back((time, Instant::now()));
    }

    fn output(&mut self, time: i64) {
        self.sent += 1;
        while let Some(&(t, at)) = self.in_flight.front() {
            if t > time {
                break;
            }
            self.in_flight.pop_front();
            if t == time {
                let latency = at.elapsed();
                self.latency += latency;
                self.latency_max = self.latency_max.max(latency);
            }
        }
    }

    fn report(&mut self) {
        let elapsed = self.since.elapsed();
        if elapsed < STATS_EVERY {
            return;
        }
        if tracing::enabled!(tracing::Level::DEBUG) {
            let secs = elapsed.as_secs_f64();
            tracing::debug!(
                "pipeline: {:.0} captured/s, {:.0} encoded/s, {:.0} out/s, {} dropped, encoder latency avg {:.1} ms max {:.1} ms, {} in flight, waited {:.0}% for the encoder",
                self.captured as f64 / secs,
                self.encoded as f64 / secs,
                self.sent as f64 / secs,
                self.dropped,
                self.latency.as_secs_f64() * 1000.0 / self.sent.max(1) as f64,
                self.latency_max.as_secs_f64() * 1000.0,
                self.in_flight.len(),
                self.waiting.as_secs_f64() * 100.0 / secs,
            );
        }
        let in_flight = std::mem::take(&mut self.in_flight);
        *self = Self::new();
        self.in_flight = in_flight;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_only_for_big_changes_and_not_too_often() {
        let long = Duration::from_secs(10);
        // Changes the encoder could live with.
        assert!(!restart_due(8_000_000, 7_000_000, long));
        assert!(!restart_due(8_000_000, 11_000_000, long));
        // A drop the connection can't carry is handled quickly...
        assert!(restart_due(8_000_000, 4_000_000, Duration::from_secs(1)));
        assert!(!restart_due(8_000_000, 4_000_000, Duration::from_millis(500)));
        // ...while a rise waits longer.
        assert!(!restart_due(8_000_000, 16_000_000, Duration::from_secs(1)));
        assert!(restart_due(8_000_000, 16_000_000, Duration::from_secs(5)));
    }
}
