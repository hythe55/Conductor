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
}

export interface Device {
  id: string
  name: string
  dnsName: string
  address: string
  os: string
  online: boolean
  host: HostInfo | null
  isSelf: boolean
  isYou: boolean
}

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
