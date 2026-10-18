//! One WebRTC session: SDP negotiation, the UDP loop, and wiring between the
//! client's control channel, the capture pipeline and input injection.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use conductor_proto::{COPIED_CHANNEL, ClientMsg, FILE_CHANNEL, FileReply, HostMsg, Route, ShownWindow};
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::format::{Codec, PayloadParams};
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::Extension;
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::audio::{self, Audio};
use crate::cursor::Cursor;
use crate::encoder::Profile;
use crate::files::{Hidden, Incoming, Outgoing};
use crate::indicator::{Area, Banner};
use crate::power::KeepAwake;
use crate::input::{Injector, Pen};
use crate::pipeline::{Config, Control, Pipeline, PipelineEvent};
use crate::quality::Quality;
use crate::state::AppState;
use crate::window;

const START_BITRATE: u32 = 8_000_000;
const MIN_BITRATE: u32 = 500_000;
const MAX_BITRATE: u32 = 40_000_000;
/// No ICE traffic for this long ends the session.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// How many due timeouts to handle in a row before checking the socket.
const MAX_DUE_TIMEOUTS: u32 = 256;
const STATS_EVERY: Duration = Duration::from_secs(5);
/// How often to check the pointer's shape.
const TICK: Duration = Duration::from_millis(50);
/// Check for something newly copied on this PC every this many ticks.
const CLIPBOARD_TICKS: u32 = 5;
/// Clipboard text bigger than this isn't shared. Data channel messages can be
/// up to 256 KiB both ways, and JSON escaping can make text longer.
const CLIPBOARD_MAX: usize = 120 * 1024;
/// Reads of one thing copied before giving up on it.
const CLIPBOARD_TRIES: u32 = 8;
/// How much the data channels may queue in all. Files waiting to go out use
/// at most a quarter, so messages on the control channel always fit.
const CHANNELS_QUEUED_MAX: usize = 4 << 20;
/// When the picture fails, try again after this long, doubling each time up
/// to the most. Capture often fails for a moment, e.g. while Windows switches
/// desktops.
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MOST: Duration = Duration::from_secs(16);
/// How often to check whether the connection goes straight to the client or
/// through a relay. Tailscale often starts relayed and finds a direct path
/// a few seconds later.
const ROUTE_EVERY: Duration = Duration::from_secs(5);

pub struct Params {
    /// Name of the device asking to control this PC, for the on-screen banner.
    pub controller: String,
    /// The client's tailnet address.
    pub peer: IpAddr,
    pub offer: String,
    pub display: Option<String>,
    pub max_size: (u32, u32),
    pub sharp: bool,
}

/// Accepts an SDP offer, spawns the session task and returns the SDP answer.
pub async fn start(state: Arc<AppState>, bind_ip: IpAddr, params: Params) -> Result<String> {
    let offer = SdpOffer::from_sdp_string(&params.offer).context("bad SDP offer")?;
    let socket = UdpSocket::bind(SocketAddr::new(bind_ip, 0)).await.context("binding UDP")?;
    let local = socket.local_addr()?;

    let mut rtc = RtcConfig::new()
        .set_ice_lite(true)
        .clear_codecs()
        .enable_h264(true)
        .enable_opus(true, false)
        .set_extension(6, Extension::PlayoutDelay)
        .enable_bwe(Some(Bitrate::bps(START_BITRATE as u64)))
        .set_sctp_max_buffered_amount(CHANNELS_QUEUED_MAX)
        .build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp")?);
    let answer = rtc.sdp_api().accept_offer(offer).context("rejecting offer")?;
    rtc.bwe().set_desired_bitrate(Bitrate::bps(MAX_BITRATE as u64));

    tokio::spawn(async move {
        let _guard = state.session_started(params.controller.clone());
        let (route_tx, route_rx) = mpsc::channel(1);
        tokio::spawn(watch_route(state.clone(), params.peer, route_tx));
        if let Err(e) = run(rtc, socket, params, route_rx).await {
            tracing::warn!("session ended: {e:#}");
        } else {
            tracing::info!("session ended");
        }
    });
    Ok(answer.to_sdp_string())
}

struct Session {
    rtc: Rtc,
    video: Option<(Mid, PayloadParams)>,
    audio: Option<(Mid, PayloadParams)>,
    /// Sound being captured, while the client wants it.
    sound: Option<Audio>,
    wants_sound: bool,
    sound_tx: mpsc::Sender<audio::Packet>,
    /// How the connection travels, told to the client when its channel opens.
    route: Option<Route>,
    control: Option<ChannelId>,
    pipeline: Option<Pipeline>,
    events_tx: mpsc::Sender<PipelineEvent>,
    injector: Injector,
    display: Option<String>,
    /// The one window shown instead of the whole display.
    window: Option<window::Handle>,
    controller: String,
    banner: Option<Banner>,
    /// Cover this PC's displays while the session lasts.
    hide_screen: bool,
    awake: Option<KeepAwake>,
    max_size: (u32, u32),
    bitrate: u32,
    /// What the connection carries now, without [`Session::bitrate`]'s smoothing.
    carried: u32,
    quality: Quality,
    /// Pixels in the picture at full size, once it has started.
    full_pixels: u64,
    connected: bool,
    paused: bool,
    /// When to try starting the picture again after it failed.
    retry_at: Option<Instant>,
    retry_after: Duration,
    /// Failures in a row. The first is retried before the client hears of it.
    failures: u32,
    stats: Stats,
    clipboard: SharedClipboard,
    cursor: Cursor,
    incoming: Incoming,
    outgoing: Outgoing,
    /// Hears how putting pasted files or pictures on the clipboard went.
    pasted_tx: mpsc::Sender<PasteDone>,
}

/// A paste that's on the clipboard, or failed to get there.
struct PasteDone {
    id: u32,
    /// Press Ctrl+V once it's there.
    keys: bool,
    result: Result<()>,
}

/// Tells the session how the connection travels, whenever that changes, until the session ends.
async fn watch_route(state: Arc<AppState>, peer: IpAddr, tx: mpsc::Sender<Route>) {
    let mut last = None;
    loop {
        if let Some(route) = state.route_to(peer).await
            && last.as_ref() != Some(&route)
        {
            last = Some(route.clone());
            if tx.send(route).await.is_err() {
                return;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(ROUTE_EVERY) => {}
            _ = tx.closed() => return,
        }
    }
}

async fn run(rtc: Rtc, socket: UdpSocket, params: Params, mut route_rx: mpsc::Receiver<Route>) -> Result<()> {
    let local = socket.local_addr()?;
    let (events_tx, mut events_rx) = mpsc::channel(32);
    let (sound_tx, mut sound_rx) = mpsc::channel(16);
    // Files copied on this PC, read as the signed-in user, with the clipboard sequence number they go with.
    let (hidden_tx, mut hidden_rx) = mpsc::channel(4);
    let (pasted_tx, mut pasted_rx) = mpsc::channel(4);
    let mut s = Session {
        rtc,
        video: None,
        audio: None,
        sound: None,
        wants_sound: false,
        sound_tx,
        route: None,
        control: None,
        pipeline: None,
        events_tx,
        injector: Injector::new(),
        display: params.display,
        window: None,
        controller: params.controller,
        banner: None,
        hide_screen: false,
        awake: None,
        max_size: params.max_size,
        bitrate: START_BITRATE,
        carried: START_BITRATE,
        quality: Quality::new(params.sharp, Instant::now()),
        full_pixels: 0,
        connected: false,
        paused: false,
        retry_at: None,
        retry_after: RETRY_FIRST,
        failures: 0,
        stats: Stats::new(),
        clipboard: SharedClipboard::new(),
        cursor: Cursor::new(),
        incoming: Incoming::default(),
        outgoing: Outgoing::default(),
        pasted_tx,
    };
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks = 0u32;
    let mut buf = vec![0u8; 2048];
    let mut last_rx = Instant::now();

    loop {
        // str0m stamps outgoing packets with the time of the last input, and the
        // congestion controller reads stale stamps as network delay. Keep it current.
        s.rtc.handle_input(Input::Timeout(Instant::now()))?;
        let mut due = 0;
        let timeout = loop {
            match s.rtc.poll_output()? {
                Output::Timeout(t) => {
                    // The pacer releases one packet per timeout and asks for the next one
                    // straight away. Going through the runtime's timer would cost a
                    // millisecond or more each time and cap the stream at a few hundred
                    // packets a second, so drive due timeouts here (bounded, so receiving
                    // still gets a turn).
                    let now = Instant::now();
                    if t > now || due == MAX_DUE_TIMEOUTS {
                        break t;
                    }
                    due += 1;
                    s.rtc.handle_input(Input::Timeout(now))?;
                }
                Output::Transmit(t) => {
                    s.stats.packets += 1;
                    s.stats.sent += t.contents.len() as u64;
                    _ = socket.send_to(&t.contents, t.destination).await;
                }
                Output::Event(e) => {
                    if !s.handle_event(e)? {
                        return Ok(());
                    }
                }
            }
        };
        s.stats.report(s.bitrate);
        if last_rx.elapsed() > IDLE_TIMEOUT {
            anyhow::bail!("client stopped responding");
        }
        s.outgoing.measure(&mut s.rtc);

        tokio::select! {
            r = socket.recv_from(&mut buf) => {
                let (n, source) = match r {
                    Ok(v) => v,
                    // Windows reports an earlier ICMP "port unreachable" as a reset on the
                    // next receive. The peer may just be changing networks, so keep going.
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                    Err(e) => return Err(e.into()),
                };
                last_rx = Instant::now();
                let Ok(contents) = buf[..n].try_into() else { continue };
                s.rtc.handle_input(Input::Receive(
                    Instant::now(),
                    Receive { proto: Protocol::Udp, source, destination: local, contents },
                ))?;
            }
            Some(ev) = events_rx.recv() => s.handle_pipeline(ev)?,
            Some(p) = sound_rx.recv() => s.send_sound(p)?,
            Some(route) = route_rx.recv() => {
                tracing::info!("route to the client: {route:?}");
                s.send(&HostMsg::Route { route: route.clone() });
                s.route = Some(route);
            }
            (id, message) = s.outgoing.next() => s.send_copied(id, message),
            _ = tick.tick() => {
                ticks = ticks.wrapping_add(1);
                s.injector.tick();
                if s.quality.update(s.carried, s.full_pixels, Instant::now()) {
                    s.requality();
                }
                if s.retry_at.is_some_and(|t| t <= Instant::now()) {
                    s.retry_at = None;
                    s.start_pipeline();
                }
                if s.control.is_some() && !s.paused {
                    if let Some(name) = s.cursor.changed() {
                        s.send(&HostMsg::Cursor { name: name.into() });
                    }
                }
                if ticks % CLIPBOARD_TICKS == 0 {
                    match s.clipboard.copied() {
                        Some(Copied::Text(text)) => {
                            s.send(&HostMsg::Clipboard { text });
                            s.withdraw_files();
                        }
                        Some(Copied::Files(paths)) => {
                            let msg = s.outgoing.offer(paths);
                            s.send(&msg);
                        }
                        Some(Copied::Other) => {
                            s.withdraw_files();
                            // They may be files that Windows hides from SYSTEM.
                            let (tx, seen) = (hidden_tx.clone(), s.clipboard.seen);
                            tokio::task::spawn_blocking(move || {
                                _ = tx.blocking_send((seen, crate::files::copied_as_user()));
                            });
                        }
                        None => {}
                    }
                }
            }
            Some((seen, hidden)) = hidden_rx.recv() => match hidden {
                // Unless something newer was copied meanwhile.
                Ok(Some(hidden)) if seen == s.clipboard.seen => {
                    let msg = match hidden {
                        Hidden::Files(paths) => s.outgoing.offer(paths),
                        Hidden::Image(png) => s.outgoing.offer_image(png),
                    };
                    s.send(&msg);
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("reading what was copied as the signed-in user: {e:#}"),
            },
            Some(done) = pasted_rx.recv() => s.pasted(done),
            _ = tokio::time::sleep_until(timeout.into()) => {}
        }
    }
}

impl Session {
    /// Returns false when the session should end.
    fn handle_event(&mut self, e: Event) -> Result<bool> {
        match e {
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return Ok(false),
            Event::Connected => {
                self.connected = true;
                tracing::info!("{} connected", self.controller);
                let banner = Banner::new(self.controller.clone());
                banner.hide_screen(self.hide_screen);
                self.banner = Some(banner);
                self.awake = KeepAwake::new(&self.controller);
                self.start_pipeline();
                self.start_sound();
            }
            Event::MediaAdded(m) if m.kind == MediaKind::Video => {
                if let Some(w) = self.rtc.writer(m.mid) {
                    let best = w
                        .payload_params()
                        .filter(|p| p.spec().codec == Codec::H264 && p.spec().format.packetization_mode == Some(1))
                        .max_by_key(|p| h264_rank(p.spec().format.profile_level_id))
                        .cloned();
                    if let Some(p) = best {
                        self.video = Some((m.mid, p));
                    }
                }
                self.start_pipeline();
            }
            Event::MediaAdded(m) if m.kind == MediaKind::Audio => {
                if let Some(w) = self.rtc.writer(m.mid) {
                    let opus = w.payload_params().find(|p| p.spec().codec == Codec::Opus).cloned();
                    self.audio = opus.map(|p| (m.mid, p));
                }
                self.start_sound();
            }
            Event::ChannelOpen(id, label) => {
                if label == "control" {
                    self.control = Some(id);
                    if let Some(route) = self.route.clone() {
                        self.send(&HostMsg::Route { route });
                    }
                } else if label == FILE_CHANNEL {
                    self.incoming.open(id);
                } else if label == COPIED_CHANNEL {
                    self.outgoing.open(id);
                }
            }
            // The client closed its connection on purpose.
            Event::ChannelClose(id) if Some(id) == self.control => return Ok(false),
            Event::ChannelClose(id) => {
                self.incoming.close(id);
                self.outgoing.close(id);
            }
            Event::ChannelData(d) if self.incoming.has(d.id) => {
                if let Some(reply) = self.incoming.receive(d.id, d.binary, &d.data) {
                    self.reply(d.id, &reply);
                }
            }
            Event::ChannelData(d) if self.outgoing.has(d.id) => {
                if let Some(reply) = self.outgoing.request(d.id, &d.data) {
                    self.reply(d.id, &reply);
                }
            }
            Event::ChannelData(d) if Some(d.id) == self.control => {
                match serde_json::from_slice::<ClientMsg>(&d.data) {
                    Ok(msg) => self.handle_client(msg),
                    Err(e) => tracing::debug!("bad control message: {e}"),
                }
            }
            Event::KeyframeRequest(_) => {
                if let Some(p) = &self.pipeline {
                    p.send(Control::Keyframe);
                }
            }
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate, .. }) => {
                self.stats.estimate = estimate.as_u64();
                let target = ((estimate.as_u64() as f64) * 0.85) as u32;
                let target = target.clamp(MIN_BITRATE, MAX_BITRATE);
                self.carried = target;
                // Ignore jitter; re-tune only on meaningful changes.
                if (target as f64 - self.bitrate as f64).abs() / self.bitrate as f64 > 0.1 {
                    self.bitrate = target;
                    if let Some(p) = &self.pipeline {
                        p.send(Control::Bitrate(target));
                    }
                }
            }
            _ => {}
        }
        Ok(true)
    }

    fn start_pipeline(&mut self) {
        if self.pipeline.is_some() || !self.connected || self.paused {
            return;
        }
        let Some((_, params)) = &self.video else { return };
        let profile = if h264_rank(params.spec().format.profile_level_id) == 3 {
            Profile::High
        } else {
            Profile::ConstrainedBaseline
        };
        self.pipeline = Some(Pipeline::start(
            Config {
                display: self.display.clone(),
                window: self.window,
                max_size: self.max_size,
                scale: self.quality.scale(),
                fps: self.quality.fps(),
                bitrate: self.bitrate,
                profile,
            },
            self.events_tx.clone(),
        ));
    }

    fn start_sound(&mut self) {
        if self.wants_sound && self.sound.is_none() && self.connected && self.audio.is_some() {
            self.sound = Some(Audio::start(self.sound_tx.clone()));
        }
    }

    fn send_sound(&mut self, p: audio::Packet) -> Result<()> {
        let Some((mid, params)) = &self.audio else { return Ok(()) };
        let pt = params.pt();
        let Some(w) = self.rtc.writer(*mid) else { return Ok(()) };
        let rtp = MediaTime::new(p.time, Frequency::FORTY_EIGHT_KHZ);
        w.write(pt, Instant::now(), rtp, p.data)?;
        Ok(())
    }

    fn restart_pipeline(&mut self) {
        self.pipeline = None;
        self.start_pipeline();
    }

    /// The picture gives way differently now, or no longer has to.
    fn requality(&mut self) {
        tracing::info!(
            "at {:.1} Mbps, sending {:.0}% of the size at {} fps",
            self.carried as f64 / 1e6,
            self.quality.scale() * 100.0,
            self.quality.fps()
        );
        if self.pipeline.is_some() {
            self.restart_pipeline();
        }
    }

    fn handle_client(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::MouseMove { x, y } => self.injector.mouse_move(x, y),
            ClientMsg::MouseDelta { dx, dy } => self.injector.mouse_delta(dx, dy),
            ClientMsg::MouseButton { button, down } => self.injector.button(button, down),
            ClientMsg::Wheel { dx, dy } => self.injector.wheel(dx, dy),
            ClientMsg::Pen { x, y, contact, pressure, tilt_x, tilt_y, twist, barrel, eraser } => {
                self.injector.pen(Pen { x, y, contact, pressure, tilt_x, tilt_y, twist, barrel, eraser })
            }
            ClientMsg::PenLeave => self.injector.pen_leave(),
            ClientMsg::Key { code, down } => self.injector.key(&code, down),
            ClientMsg::Text { text } => self.injector.text(&text),
            ClientMsg::ReleaseAll => self.injector.release_all(),
            ClientMsg::SelectDisplay { id } => {
                if self.display.as_deref() != Some(id.as_str()) || self.window.is_some() {
                    self.display = Some(id);
                    self.window = None;
                    self.restart_pipeline();
                }
            }
            ClientMsg::SelectWindow { id } => match window::from_id(&id) {
                Some(w) => {
                    window::bring_forward(w);
                    self.window = Some(w);
                    self.restart_pipeline();
                }
                // Closed since it was listed.
                None => self.send(&HostMsg::Windows { windows: window::list() }),
            },
            ClientMsg::ListWindows => self.send(&HostMsg::Windows { windows: window::list() }),
            ClientMsg::RequestKeyframe => {
                if let Some(p) = &self.pipeline {
                    p.send(Control::Keyframe);
                }
            }
            ClientMsg::SetPaused { paused } => {
                self.paused = paused;
                if paused {
                    self.injector.release_all();
                    self.pipeline = None;
                } else {
                    self.start_pipeline();
                }
            }
            ClientMsg::HideScreen { hide } => {
                self.hide_screen = hide;
                if let Some(b) = &self.banner {
                    b.hide_screen(hide);
                }
            }
            ClientMsg::SetSharp { sharp } => {
                if self.quality.set_sharp(sharp, self.carried, self.full_pixels) {
                    self.requality();
                }
            }
            ClientMsg::SetAudio { enabled } => {
                self.wants_sound = enabled;
                if enabled {
                    self.start_sound();
                } else {
                    self.sound = None;
                }
            }
            ClientMsg::Clipboard { text } => {
                if self.clipboard.paste(text) {
                    self.withdraw_files();
                }
            }
            ClientMsg::Paste { id, image, keys } => {
                let Some(pasted) = self.incoming.take_paste(id) else {
                    let message = Some("Nothing arrived to paste.".into());
                    self.send(&HostMsg::Pasted { id, message });
                    return;
                };
                // What goes on the clipboard now came from the client.
                self.clipboard.pasting += 1;
                let tx = self.pasted_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = crate::files::paste(&pasted, image);
                    // The clipboard has its own copy of a picture; files stay until pasted.
                    if image || result.is_err() {
                        _ = std::fs::remove_dir_all(&pasted.folder);
                    }
                    _ = tx.blocking_send(PasteDone { id, keys, result });
                });
            }
        }
    }

    /// Something the client pasted is on this PC's clipboard, or failed to get there.
    fn pasted(&mut self, done: PasteDone) {
        self.clipboard.pasted();
        let message = match done.result {
            Ok(()) => {
                tracing::info!("pasted from the client");
                self.withdraw_files();
                if done.keys {
                    self.injector.paste();
                }
                None
            }
            Err(e) => {
                tracing::warn!("putting what the client pasted on the clipboard: {e:#}");
                Some(format!("{e:#}"))
            }
        };
        self.send(&HostMsg::Pasted { id: done.id, message });
    }

    fn send(&mut self, msg: &HostMsg) {
        let Some(id) = self.control else { return };
        let Some(mut ch) = self.rtc.channel(id) else { return };
        if let Ok(json) = serde_json::to_vec(msg) {
            _ = ch.write(false, &json);
        }
    }

    /// Answers on a file channel.
    fn reply(&mut self, id: ChannelId, reply: &FileReply) {
        if let (Some(mut ch), Ok(json)) = (self.rtc.channel(id), serde_json::to_vec(reply)) {
            _ = ch.write(false, &json);
        }
    }

    /// Files copied on this PC are no longer on its clipboard.
    fn withdraw_files(&mut self) {
        if let Some(msg) = self.outgoing.withdraw() {
            self.send(&msg);
        }
    }

    /// Passes on part of a copied file the client is fetching.
    fn send_copied(&mut self, id: ChannelId, message: Option<(bool, Vec<u8>)>) {
        let Some((binary, data)) = message else {
            self.outgoing.read(id);
            return;
        };
        let written = self.rtc.channel(id).map(|mut ch| ch.write(binary, &data));
        if !matches!(written, Some(Ok(true))) {
            // Measuring before each message should leave room for it.
            tracing::warn!("couldn't queue part of a copied file: {written:?}");
            self.outgoing.close(id);
            self.rtc.direct_api().close_data_channel(id);
        }
    }

    fn handle_pipeline(&mut self, ev: PipelineEvent) -> Result<()> {
        match ev {
            PipelineEvent::Frame(f) => {
                let Some((mid, params)) = &self.video else { return Ok(()) };
                let pt = params.pt();
                let Some(w) = self.rtc.writer(*mid) else { return Ok(()) };
                let rtp = MediaTime::new((f.time as u64) * 9 / 1000, Frequency::NINETY_KHZ);
                let zero = MediaTime::new(0, Frequency::NINETY_KHZ);
                self.stats.frames += 1;
                self.stats.written += f.data.len() as u64;
                w.playout_delay(zero, zero).write(pt, Instant::now(), rtp, f.data)?;
            }
            PipelineEvent::Started { display, width, height, encoder, origin, size, view, window } => {
                let id = display.clone();
                match window {
                    Some(_) => tracing::info!("streaming a window on {id} at {width}x{height} via {encoder}"),
                    None => tracing::info!("streaming {id} at {width}x{height} via {encoder}"),
                }
                self.injector.set_display((view.x, view.y), (view.width, view.height));
                let full = crate::convert::fit((view.width, view.height), self.max_size);
                self.full_pixels = full.0 as u64 * full.1 as u64;
                if let Some(b) = &self.banner {
                    b.set_area(Area { x: origin.0, y: origin.1, width: size.0, height: size.1 });
                }
                self.display = Some(display.clone());
                self.window = window;
                self.failures = 0;
                self.retry_after = RETRY_FIRST;
                let displays = crate::capture::displays().unwrap_or_default();
                let window = window.and_then(window::info).map(|w| ShownWindow {
                    id: w.id,
                    title: w.title,
                    width: view.width,
                    height: view.height,
                });
                self.send(&HostMsg::Displays { displays, active: display, window });
            }
            PipelineEvent::Moved { view } => self.injector.set_display((view.x, view.y), (view.width, view.height)),
            PipelineEvent::Failed(message) => {
                self.pipeline = None;
                self.failures += 1;
                if self.failures > 1 {
                    self.send(&HostMsg::Error { message });
                }
                self.retry_at = Some(Instant::now() + self.retry_after);
                self.retry_after = (self.retry_after * 2).min(RETRY_MOST);
            }
        }
        Ok(())
    }
}

/// Counts what the session sent, logged every few seconds at debug level.
struct Stats {
    since: Instant,
    frames: u32,
    /// Encoded video handed to str0m.
    written: u64,
    packets: u32,
    /// Everything str0m put on the wire.
    sent: u64,
    estimate: u64,
}

impl Stats {
    fn new() -> Self {
        Self { since: Instant::now(), frames: 0, written: 0, packets: 0, sent: 0, estimate: 0 }
    }

    fn report(&mut self, bitrate: u32) {
        let elapsed = self.since.elapsed();
        if elapsed < STATS_EVERY {
            return;
        }
        let secs = elapsed.as_secs_f64();
        tracing::debug!(
            "session: {:.0} frames/s, {:.2} Mbps of video, {:.0} packets/s, {:.2} Mbps sent, estimate {:.2} Mbps, encoder target {:.2} Mbps",
            self.frames as f64 / secs,
            self.written as f64 * 8.0 / secs / 1e6,
            self.packets as f64 / secs,
            self.sent as f64 * 8.0 / secs / 1e6,
            self.estimate as f64 / 1e6,
            bitrate as f64 / 1e6,
        );
        *self = Self { estimate: self.estimate, ..Self::new() };
    }
}

/// Something newly copied on this PC.
enum Copied {
    Text(String),
    Files(Vec<PathBuf>),
    /// Something else, or text too big to share.
    Other,
}

/// Keeps this PC's clipboard and the client's in step, for text, and notices
/// copied files.
struct SharedClipboard {
    /// Clipboard sequence number last looked at.
    seen: u32,
    /// The text both sides have, so it isn't sent back where it came from.
    shared: Option<String>,
    /// Failed reads of what's on the clipboard, by its sequence number.
    failures: (u32, u32),
    /// Pastes from the client on their way onto the clipboard.
    pasting: u32,
}

impl SharedClipboard {
    fn new() -> Self {
        // Only what's copied from now on is shared, not what was there before.
        Self { seen: conductor_clipboard::sequence(), shared: None, failures: (0, 0), pasting: 0 }
    }

    /// What was copied on this PC since the last call, unless it's text the
    /// client already has.
    fn copied(&mut self) -> Option<Copied> {
        let now = conductor_clipboard::sequence();
        if now == self.seen || self.pasting > 0 {
            return None;
        }
        let read = conductor_clipboard::read_files().and_then(|files| match files {
            Some(files) => Ok(Copied::Files(files)),
            None => Ok(conductor_clipboard::read_text(CLIPBOARD_MAX)?.map_or(Copied::Other, Copied::Text)),
        });
        if self.failures.0 != now {
            self.failures = (now, 0);
        }
        let copied = match read {
            Ok(copied) => copied,
            // Another app may be holding it; try again next time, for a while.
            Err(e) if self.failures.1 < CLIPBOARD_TRIES => {
                if self.failures.1 == 0 {
                    tracing::debug!("reading the clipboard: {e:#}");
                }
                self.failures.1 += 1;
                return None;
            }
            Err(e) => {
                tracing::warn!("couldn't read the clipboard: {e:#}");
                Copied::Other
            }
        };
        if matches!(copied, Copied::Other) {
            let formats = conductor_clipboard::formats().unwrap_or_default();
            tracing::debug!("copied something that isn't text or files here (formats {formats:?})");
        }
        self.seen = now;
        match copied {
            Copied::Text(text) if self.shared.as_ref() == Some(&text) => None,
            Copied::Text(text) => {
                self.shared = Some(text.clone());
                Some(Copied::Text(text))
            }
            other => {
                self.shared = None;
                Some(other)
            }
        }
    }

    /// A paste from the client is on the clipboard, or failed to get there.
    /// Either way, what's there now isn't news to the client.
    fn pasted(&mut self) {
        self.pasting = self.pasting.saturating_sub(1);
        self.seen = conductor_clipboard::sequence();
        self.shared = None;
    }

    /// Puts text copied on the client on this PC's clipboard. Returns whether
    /// the clipboard holds it now.
    fn paste(&mut self, text: String) -> bool {
        if text.len() > CLIPBOARD_MAX {
            return false;
        }
        if self.shared.as_ref() == Some(&text) {
            return true;
        }
        match conductor_clipboard::write_text(&text) {
            Ok(()) => {
                self.seen = conductor_clipboard::sequence();
                self.shared = Some(text);
                true
            }
            Err(e) => {
                tracing::warn!("couldn't set the clipboard: {e:#}");
                false
            }
        }
    }
}

/// Higher is better: High > Main > Constrained Baseline > Baseline.
fn h264_rank(profile_level_id: Option<u32>) -> u8 {
    match profile_level_id.map(|p| p >> 8) {
        Some(0x6400) => 3,
        Some(0x4d00) => 2,
        Some(0x42e0) => 1,
        _ => 0,
    }
}
