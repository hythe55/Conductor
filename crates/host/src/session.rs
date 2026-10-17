//! One WebRTC session: SDP negotiation, the UDP loop, and wiring between the
//! client's control channel, the capture pipeline and input injection.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use conductor_proto::{ClientMsg, HostMsg};
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::format::Codec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::Extension;
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::encoder::Profile;
use crate::indicator::{Area, Banner};
use crate::input::Injector;
use crate::pipeline::{Config, Control, Pipeline, PipelineEvent};
use crate::state::AppState;

const START_BITRATE: u32 = 8_000_000;
const MIN_BITRATE: u32 = 500_000;
const MAX_BITRATE: u32 = 40_000_000;
/// No ICE traffic for this long ends the session.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Params {
    /// Name of the device asking to control this PC, for the on-screen banner.
    pub controller: String,
    pub offer: String,
    pub display: Option<String>,
    pub max_size: (u32, u32),
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
        .build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp")?);
    let answer = rtc.sdp_api().accept_offer(offer).context("rejecting offer")?;
    rtc.bwe().set_desired_bitrate(Bitrate::bps(MAX_BITRATE as u64));

    tokio::spawn(async move {
        let _guard = state.session_started(params.controller.clone());
        if let Err(e) = run(rtc, socket, params).await {
            tracing::warn!("session ended: {e:#}");
        } else {
            tracing::info!("session ended");
        }
    });
    Ok(answer.to_sdp_string())
}

struct Session {
    rtc: Rtc,
    video: Option<(Mid, str0m::format::PayloadParams)>,
    control: Option<ChannelId>,
    pipeline: Option<Pipeline>,
    events_tx: mpsc::Sender<PipelineEvent>,
    injector: Injector,
    display: Option<String>,
    controller: String,
    banner: Option<Banner>,
    max_size: (u32, u32),
    bitrate: u32,
    connected: bool,
    paused: bool,
}

async fn run(rtc: Rtc, socket: UdpSocket, params: Params) -> Result<()> {
    let local = socket.local_addr()?;
    let (events_tx, mut events_rx) = mpsc::channel(32);
    let mut s = Session {
        rtc,
        video: None,
        control: None,
        pipeline: None,
        events_tx,
        injector: Injector::new(),
        display: params.display,
        controller: params.controller,
        banner: None,
        max_size: params.max_size,
        bitrate: START_BITRATE,
        connected: false,
        paused: false,
    };
    let mut buf = vec![0u8; 2048];
    let mut last_rx = Instant::now();

    loop {
        let timeout = loop {
            match s.rtc.poll_output()? {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => {
                    _ = socket.send_to(&t.contents, t.destination).await;
                }
                Output::Event(e) => {
                    if !s.handle_event(e)? {
                        return Ok(());
                    }
                }
            }
        };
        if last_rx.elapsed() > IDLE_TIMEOUT {
            anyhow::bail!("client stopped responding");
        }

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
            _ = tokio::time::sleep_until(timeout.into()) => {
                s.rtc.handle_input(Input::Timeout(Instant::now()))?;
            }
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
                self.banner = Some(Banner::new(self.controller.clone()));
                self.start_pipeline();
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
            Event::ChannelOpen(id, label) => {
                if label == "control" {
                    self.control = Some(id);
                }
            }
            // The client closed its connection on purpose.
            Event::ChannelClose(id) if Some(id) == self.control => return Ok(false),
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
                let target = ((estimate.as_u64() as f64) * 0.85) as u32;
                let target = target.clamp(MIN_BITRATE, MAX_BITRATE);
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
                max_size: self.max_size,
                fps: 60,
                bitrate: self.bitrate,
                profile,
            },
            self.events_tx.clone(),
        ));
    }

    fn restart_pipeline(&mut self) {
        self.pipeline = None;
        self.start_pipeline();
    }

    fn handle_client(&mut self, msg: ClientMsg) {
        match msg {
            ClientMsg::MouseMove { x, y } => self.injector.mouse_move(x, y),
            ClientMsg::MouseDelta { dx, dy } => self.injector.mouse_delta(dx, dy),
            ClientMsg::MouseButton { button, down } => self.injector.button(button, down),
            ClientMsg::Wheel { dx, dy } => self.injector.wheel(dx, dy),
            ClientMsg::Key { code, down } => self.injector.key(&code, down),
            ClientMsg::Text { text } => self.injector.text(&text),
            ClientMsg::ReleaseAll => self.injector.release_all(),
            ClientMsg::SelectDisplay { id } => {
                if self.display.as_deref() != Some(id.as_str()) {
                    self.display = Some(id);
                    self.restart_pipeline();
                }
            }
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
        }
    }

    fn send(&mut self, msg: &HostMsg) {
        let Some(id) = self.control else { return };
        let Some(mut ch) = self.rtc.channel(id) else { return };
        if let Ok(json) = serde_json::to_vec(msg) {
            _ = ch.write(false, &json);
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
                w.playout_delay(zero, zero).write(pt, Instant::now(), rtp, f.data)?;
            }
            PipelineEvent::Started { display, width, height, encoder, origin, size } => {
                let id = display.clone();
                tracing::info!("streaming {id} at {width}x{height} via {encoder}");
                self.injector.set_display(origin, size);
                if let Some(b) = &self.banner {
                    b.set_area(Area { x: origin.0, y: origin.1, width: size.0, height: size.1 });
                }
                self.display = Some(display.clone());
                let displays = crate::capture::displays().unwrap_or_default();
                self.send(&HostMsg::Displays { displays, active: display });
            }
            PipelineEvent::Failed(message) => {
                self.pipeline = None;
                self.send(&HostMsg::Error { message });
            }
        }
        Ok(())
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
