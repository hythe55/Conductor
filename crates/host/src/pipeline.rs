//! Capture, convert and encode one display on a dedicated thread.
//!
//! Runs only while a client is connected. A static screen produces no frames
//! after a short refinement burst, so an idle session sends almost nothing.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use tokio::sync::mpsc as tmpsc;
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

use crate::capture::{Capture, Frame, ResolutionChanged};
use crate::convert::{Converter, fit};
use crate::encoder::{EncodedFrame, Encoder, Profile};
use crate::indicator::Area;
use crate::window;

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
/// How often a shown window is checked for having moved.
const FOLLOW_EVERY: Duration = Duration::from_millis(100);
/// A shown window that changed size or display is shown again once it has
/// stayed put this long, rather than restarting the encoder all through a drag.
const SETTLE: Duration = Duration::from_millis(300);
/// How long a window being restored from the taskbar gets to come back.
const RESTORE_WAIT: Duration = Duration::from_secs(1);
/// Video handed on can get this far ahead of what the connection has had time
/// to carry before frames are skipped. More would only wait in the pacer's
/// queue, and a queue there shows as seconds of stutter and delay.
const MAX_BEHIND: Duration = Duration::from_millis(50);
/// What the connection carries, as a multiple of the encoder's target: the
/// session aims the encoder a little under its estimate.
const CARRIED_OVER_TARGET: f64 = 1.15;
/// Keyframes the client asks for come at most this often. Each is several times
/// bigger than other frames, so a run of them would clog the connection that
/// lost the picture in the first place.
const KEYFRAME_GAP: Duration = Duration::from_millis(500);
/// Trouble keeping up is logged at most this often.
const NOTE_EVERY: Duration = Duration::from_secs(30);

pub enum Control {
    Keyframe,
    Bitrate(u32),
}

pub struct Config {
    pub display: Option<String>,
    /// Show only this window, from the display most of it is on.
    pub window: Option<window::Handle>,
    pub max_size: (u32, u32),
    /// Width and height as parts of what fits in `max_size`, on a connection too slow for all of it.
    pub scale: f64,
    pub fps: u32,
    pub bitrate: u32,
    pub profile: Profile,
}

pub enum PipelineEvent {
    Frame(EncodedFrame),
    /// `origin` and `size` are the display's, and `view` the part of it shown:
    /// all of it, or the area of `window`. A window that closed is replaced by
    /// its display, with `window` unset.
    Started {
        display: String,
        width: u32,
        height: u32,
        encoder: String,
        origin: (i32, i32),
        size: (u32, u32),
        view: Area,
        window: Option<window::Handle>,
    },
    /// The shown window moved, without changing size or display.
    Moved { view: Area },
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
        _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    crate::encoder::startup()?;
    let start = Instant::now();

    // Rebuilt from scratch when the display mode changes, or the shown window
    // changes size or display.
    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
        let placed = config.window.and_then(|w| place(w, stop));
        match &placed {
            Some((display, _)) => config.display = Some(display.clone()),
            None if config.window.is_some() => {
                tracing::info!("the shown window closed, or was hidden or minimized; showing its display");
                config.window = None;
            }
            None => {}
        }
        let mut cap = Capture::new(config.display.as_deref())?;
        let screen = Area { x: cap.origin.0, y: cap.origin.1, width: cap.width, height: cap.height };
        let mut view = match placed {
            Some((_, view)) if contains(screen, view) => view,
            _ => {
                config.window = None;
                screen
            }
        };
        let full = fit((view.width, view.height), config.max_size);
        let size = fit(full, ((full.0 as f64 * config.scale) as u32, (full.1 as f64 * config.scale) as u32));
        let mut conv =
            Converter::new(&cap.device, &cap.context, &cap.texture, (cap.width, cap.height), within(view, screen), size)?;
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
            view,
            window: config.window,
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
        let mut followed = Instant::now();
        // A different place the shown window has been seen at, and since when.
        let mut settling: Option<(Option<Area>, Instant)> = None;

        let mut stats = Stats::new();
        let mut backlog = Backlog::new();
        // When this encoder last sent a keyframe.
        let mut last_key: Option<Instant> = None;
        let result: Result<()> = (|| {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                stats.report(config.bitrate);
                if let Some(w) = config.window
                    && followed.elapsed() >= FOLLOW_EVERY
                {
                    followed = Instant::now();
                    match window::rect(w).and_then(|r| window::crop(r, screen)) {
                        Some(now) if (now.width, now.height) == (view.width, view.height) => {
                            settling = None;
                            if now != view {
                                view = now;
                                conv.set_crop(within(view, screen));
                                dirty = true;
                                _ = out.try_send(PipelineEvent::Moved { view });
                            }
                        }
                        // Resized, moved to another display, minimized, hidden or closed.
                        now => match settling {
                            Some((seen, since)) if seen == now => {
                                if since.elapsed() >= SETTLE {
                                    bail!(ViewChanged);
                                }
                            }
                            _ => settling = Some((now, Instant::now())),
                        },
                    }
                }
                while let Ok(c) = control.try_recv() {
                    match c {
                        Control::Keyframe => {
                            stats.asked += 1;
                            want_key = true;
                        }
                        Control::Bitrate(b) => {
                            config.bitrate = b;
                            stats.target(b);
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
                                last_key = None;
                                stats.restarts += 1;
                            }
                            Ok(Err(e)) => tracing::warn!("couldn't restart the encoder: {e:#}"),
                            Err(_) => tracing::warn!("couldn't restart the encoder"),
                        }
                    }
                }
                for f in enc.poll()? {
                    stats.output(f.time);
                    let size = f.data.len();
                    if out.try_send(PipelineEvent::Frame(f)).is_err() {
                        // Network side is backed up: drop and resync with a keyframe.
                        stats.dropped += 1;
                        want_key = true;
                    } else {
                        backlog.add(size);
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
                let key_due = want_key && last_key.is_none_or(|t| t.elapsed() >= KEYFRAME_GAP);
                if !have_image || !(dirty || key_due || refine_due) {
                    continue;
                }
                if backlog.behind(config.bitrate, Instant::now()) > MAX_BEHIND {
                    // Try again at the next frame, once more has gone out. Until then the
                    // connection gets fewer frames rather than late ones.
                    stats.skipped += 1;
                    next_due = Instant::now() + frame_interval;
                    continue;
                }
                if !dirty && refine_due {
                    refine_left -= 1;
                }
                let tex = conv.convert()?;
                let time = start.elapsed().as_nanos() as i64 / 100;
                stats.submitted(time);
                if key_due {
                    want_key = false;
                    last_key = Some(Instant::now());
                    stats.keyframes += 1;
                }
                for f in enc.encode(&tex, time, key_due)? {
                    stats.output(f.time);
                    let size = f.data.len();
                    if out.try_send(PipelineEvent::Frame(f)).is_err() {
                        stats.dropped += 1;
                        want_key = true;
                    } else {
                        backlog.add(size);
                    }
                }
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
            Err(e) if e.is::<ViewChanged>() => {
                tracing::debug!("the shown window changed size or display, restarting pipeline");
                drop(enc);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Finds the shown window's display and area, waiting a moment for one
/// that's being restored from the taskbar.
fn place(w: window::Handle, stop: &std::sync::atomic::AtomicBool) -> Option<(String, Area)> {
    let until = Instant::now() + RESTORE_WAIT;
    while window::minimized(w) && Instant::now() < until && !stop.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let displays = crate::capture::displays().ok()?;
    window::place(w, &displays)
}

fn contains(outer: Area, inner: Area) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width as i32 <= outer.x + outer.width as i32
        && inner.y + inner.height as i32 <= outer.y + outer.height as i32
}

/// `view` relative to the display it's on, as the converter takes it.
fn within(view: Area, screen: Area) -> (i32, i32, u32, u32) {
    (view.x - screen.x, view.y - screen.y, view.width, view.height)
}

/// The shown window changed size or display, so the pipeline starts over.
#[derive(Debug)]
struct ViewChanged;

impl std::fmt::Display for ViewChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the shown window changed size or display")
    }
}

impl std::error::Error for ViewChanged {}

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
            unsafe { _ = CoInitializeEx(None, COINIT_MULTITHREADED) };
            _ = tx.send(Encoder::new(&device, luid, size.0, size.1, fps, bitrate, profile));
        });
        Self { bitrate, started: Instant::now(), rx }
    }
}

/// Whether an encoder running at `current` should be restarted to reach `wanted`.
fn restart_due(current: u32, wanted: u32, since_last: Duration) -> bool {
    let ratio = wanted as f64 / current as f64;
    (ratio < RESTART_BELOW && since_last >= RESTART_DOWN_AFTER)
        || (ratio > RESTART_ABOVE && since_last >= RESTART_UP_AFTER)
}

/// Encoded video handed on that the connection hasn't had time to carry yet,
/// going by the encoder's target bitrate.
struct Backlog {
    bytes: f64,
    at: Instant,
}

impl Backlog {
    fn new() -> Self {
        Self { bytes: 0.0, at: Instant::now() }
    }

    fn add(&mut self, bytes: usize) {
        self.bytes += bytes as f64;
    }

    /// How long what's left takes to go out, with the encoder aimed at `bitrate`.
    fn behind(&mut self, bitrate: u32, now: Instant) -> Duration {
        let rate = bitrate.max(1) as f64 * CARRIED_OVER_TARGET / 8.0;
        self.bytes = (self.bytes - rate * now.saturating_duration_since(self.at).as_secs_f64()).max(0.0);
        self.at = now;
        Duration::from_secs_f64(self.bytes / rate)
    }
}

/// Counts what the pipeline did, logged every few seconds at debug level, and
/// at info level when it had trouble keeping up.
struct Stats {
    since: Instant,
    captured: u32,
    encoded: u32,
    sent: u32,
    dropped: u32,
    /// Frames left out because the connection hadn't caught up.
    skipped: u32,
    keyframes: u32,
    /// Keyframes the client asked for.
    asked: u32,
    restarts: u32,
    /// Lowest and highest encoder target.
    targets: Option<(u32, u32)>,
    /// When trouble was last logged at info level.
    noted: Option<Instant>,
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
            skipped: 0,
            keyframes: 0,
            asked: 0,
            restarts: 0,
            targets: None,
            noted: None,
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

    fn target(&mut self, bitrate: u32) {
        let (low, high) = self.targets.unwrap_or((bitrate, bitrate));
        self.targets = Some((low.min(bitrate), high.max(bitrate)));
    }

    fn report(&mut self, bitrate: u32) {
        let elapsed = self.since.elapsed();
        if elapsed < STATS_EVERY {
            return;
        }
        // A few skipped frames and a keyframe or two are usual as the picture starts.
        let trouble = self.skipped * 20 > self.captured || self.dropped > 0 || self.asked > 2 || self.restarts > 0;
        if trouble && self.noted.is_none_or(|t| t.elapsed() >= NOTE_EVERY) {
            self.noted = Some(Instant::now());
            let (low, high) = self.targets.unwrap_or((bitrate, bitrate));
            tracing::info!(
                "video couldn't keep up for a moment: skipped {} of {} frames, dropped {}, {} keyframes ({} asked for), {} encoder restarts, target {:.1} to {:.1} Mbps",
                self.skipped,
                self.captured,
                self.dropped,
                self.keyframes,
                self.asked,
                self.restarts,
                low.min(bitrate) as f64 / 1e6,
                high.max(bitrate) as f64 / 1e6,
            );
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
        let noted = self.noted;
        *self = Self::new();
        self.in_flight = in_flight;
        self.noted = noted;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backlog_drains_at_a_little_over_the_target() {
        let start = Instant::now();
        let mut backlog = Backlog { bytes: 0.0, at: start };
        // A 100 KB keyframe at 8 Mbps takes 87 ms to go out.
        backlog.add(100_000);
        let behind = backlog.behind(8_000_000, start);
        assert!((86..=88).contains(&behind.as_millis()), "{behind:?}");
        assert!(backlog.behind(8_000_000, start + Duration::from_millis(50)).as_millis() < 40);
        assert_eq!(backlog.behind(8_000_000, start + Duration::from_millis(200)), Duration::ZERO);
    }

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
