import { createSession, type HostUrl, type SessionOptions } from './api'
import type { ClientMsg, HostMsg } from './types'

export type SessionState = 'connecting' | 'connected' | 'reconnecting' | 'failed' | 'closed'

export interface Stats {
  fps: number
  bitrateKbps: number
  width: number
  height: number
  rttMs: number | null
  decoder: string
  dropped: number
  lost: number
}

export interface SessionHandlers {
  onState(state: SessionState, error?: string): void
  onStream(stream: MediaStream): void
  onMessage(msg: HostMsg): void
}

const RECONNECT_DELAYS = [500, 1000, 2000, 4000, 8000]

/** Prefer H.264 High, then any H.264, then whatever else the browser has. */
function preferH264(t: RTCRtpTransceiver) {
  const caps = RTCRtpReceiver.getCapabilities?.('video')
  if (!caps || !t.setCodecPreferences) return
  const rank = (c: { mimeType: string; sdpFmtpLine?: string }) => {
    if (c.mimeType.toLowerCase() !== 'video/h264') return 0
    const fmtp = c.sdpFmtpLine ?? ''
    const packetized = fmtp.includes('packetization-mode=1') ? 2 : 0
    return 10 + packetized + (fmtp.includes('profile-level-id=64') ? 1 : 0)
  }
  const sorted = [...caps.codecs].sort((a, b) => rank(b) - rank(a))
  try {
    t.setCodecPreferences(sorted)
  } catch {
    // Older engines: fall back to the browser's default order.
  }
}

export class RemoteSession {
  private pc: RTCPeerConnection | null = null
  private channel: RTCDataChannel | null = null
  private attempt = 0
  private closed = false
  private disconnectTimer: number | undefined
  private lastStats: { at: number; bytes: number; frames: number } | null = null
  options: SessionOptions

  constructor(
    private host: HostUrl,
    options: SessionOptions,
    private handlers: SessionHandlers,
  ) {
    this.options = { ...options }
  }

  async connect(): Promise<void> {
    this.handlers.onState(this.attempt === 0 ? 'connecting' : 'reconnecting')
    this.teardown()
    const pc = new RTCPeerConnection({ iceServers: [], bundlePolicy: 'max-bundle' })
    this.pc = pc

    const video = pc.addTransceiver('video', { direction: 'recvonly' })
    preferH264(video)
    pc.addTransceiver('audio', { direction: 'recvonly' })

    const channel = pc.createDataChannel('control', { ordered: true })
    this.channel = channel
    channel.onmessage = (e) => {
      try {
        this.handlers.onMessage(JSON.parse(e.data as string) as HostMsg)
      } catch {
        // Ignore malformed messages.
      }
    }

    pc.ontrack = (e) => {
      const r = e.receiver as RTCRtpReceiver & { jitterBufferTarget?: number; playoutDelayHint?: number }
      // Lowest latency: render frames as soon as they decode.
      if ('jitterBufferTarget' in r) r.jitterBufferTarget = 0
      if ('playoutDelayHint' in r) r.playoutDelayHint = 0
      if (e.track.kind === 'video') this.handlers.onStream(e.streams[0] ?? new MediaStream([e.track]))
    }

    pc.onconnectionstatechange = () => {
      if (this.pc !== pc || this.closed) return
      switch (pc.connectionState) {
        case 'connected':
          clearTimeout(this.disconnectTimer)
          this.attempt = 0
          this.handlers.onState('connected')
          break
        case 'disconnected':
          // Brief network blips recover on their own; give ICE a moment.
          clearTimeout(this.disconnectTimer)
          this.disconnectTimer = window.setTimeout(() => this.retry(), 3000)
          this.handlers.onState('reconnecting')
          break
        case 'failed':
          this.retry()
          break
      }
    }

    try {
      const offer = await pc.createOffer()
      await pc.setLocalDescription(offer)
      const answer = await createSession(this.host, offer.sdp!, this.options)
      if (this.pc !== pc) return
      await pc.setRemoteDescription({ type: 'answer', sdp: answer })
    } catch (e) {
      if (this.pc !== pc) return
      if (this.attempt === 0) {
        this.handlers.onState('failed', e instanceof Error ? e.message : String(e))
        this.teardown()
      } else {
        this.retry()
      }
    }
  }

  private retry() {
    if (this.closed) return
    if (this.attempt >= RECONNECT_DELAYS.length) {
      this.teardown()
      this.handlers.onState('failed', 'Lost the connection and couldn’t get it back.')
      return
    }
    const delay = RECONNECT_DELAYS[this.attempt++]
    this.handlers.onState('reconnecting')
    this.teardown()
    window.setTimeout(() => {
      if (!this.closed) void this.connect()
    }, delay)
  }

  send(msg: ClientMsg) {
    if (this.channel?.readyState === 'open') this.channel.send(JSON.stringify(msg))
  }

  async stats(): Promise<Stats | null> {
    const pc = this.pc
    if (!pc) return null
    const report = await pc.getStats()
    let inbound: any = null
    let rtt: number | null = null
    report.forEach((s: any) => {
      if (s.type === 'inbound-rtp' && s.kind === 'video') inbound = s
      if (s.type === 'candidate-pair' && s.nominated && s.state === 'succeeded' && s.currentRoundTripTime != null) {
        rtt = s.currentRoundTripTime * 1000
      }
    })
    if (!inbound) return null
    const now = performance.now()
    let bitrate = 0
    let fps = inbound.framesPerSecond ?? 0
    if (this.lastStats) {
      const dt = (now - this.lastStats.at) / 1000
      bitrate = ((inbound.bytesReceived - this.lastStats.bytes) * 8) / 1000 / dt
      if (!inbound.framesPerSecond) fps = (inbound.framesDecoded - this.lastStats.frames) / dt
    }
    this.lastStats = { at: now, bytes: inbound.bytesReceived, frames: inbound.framesDecoded }
    return {
      fps: Math.round(fps),
      bitrateKbps: Math.round(bitrate),
      width: inbound.frameWidth ?? 0,
      height: inbound.frameHeight ?? 0,
      rttMs: rtt == null ? null : Math.round(rtt),
      decoder: inbound.decoderImplementation ?? '',
      dropped: inbound.framesDropped ?? 0,
      lost: inbound.packetsLost ?? 0,
    }
  }

  private teardown() {
    clearTimeout(this.disconnectTimer)
    this.channel = null
    if (this.pc) {
      this.pc.onconnectionstatechange = null
      this.pc.ontrack = null
      this.pc.close()
      this.pc = null
    }
    this.lastStats = null
  }

  close() {
    this.closed = true
    this.send({ t: 'releaseAll' })
    this.teardown()
    this.handlers.onState('closed')
  }
}
