// Mirrors crates/proto. Keep in sync.

export interface DisplayInfo {
  id: string
  name: string
  x: number
  y: number
  width: number
  height: number
  primary: boolean
  virtual: boolean
}

export interface HostInfo {
  name: string
  version: string
  protocol: number
  os: string
  displays: DisplayInfo[]
  /** Devices controlling this PC right now. */
  controllers?: string[]
}

export interface Device {
  id: string
  name: string
  dnsName: string
  address: string
  os: string
  online: boolean
  /** For offline devices, when Tailscale last heard from them (ISO 8601). */
  lastSeen?: string
  host: HostInfo | null
  isSelf: boolean
  isYou: boolean
}

export interface ControlStatus {
  hosting: boolean
  agentRunning: boolean
  version: string
  autoUpdate: boolean
  update: UpdateStatus
}

export type UpdateStatus =
  | { state: 'notChecked' | 'checking' | 'upToDate' }
  | { state: 'ready' | 'installing'; version: string }
  | { state: 'failed'; message: string }

export type MouseButton = 'left' | 'right' | 'middle' | 'back' | 'forward'

export type ClientMsg =
  | { t: 'mouseMove'; x: number; y: number }
  | { t: 'mouseDelta'; dx: number; dy: number }
  | { t: 'mouseButton'; button: MouseButton; down: boolean }
  | { t: 'wheel'; dx: number; dy: number }
  | { t: 'key'; code: string; down: boolean }
  | { t: 'text'; text: string }
  | { t: 'releaseAll' }
  | { t: 'selectDisplay'; id: string }
  | { t: 'requestKeyframe' }
  | { t: 'setPaused'; paused: boolean }

export type HostMsg =
  | { t: 'hello'; host: HostInfo; display: string }
  | { t: 'displays'; displays: DisplayInfo[]; active: string }
  | { t: 'stats'; bitrateKbps: number; fps: number; width: number; height: number; encoder: string }
  | { t: 'error'; message: string }
