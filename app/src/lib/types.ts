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

/** A window open on the remote PC. The id lasts until it closes. */
export interface WindowInfo {
  id: string
  title: string
}

/** The window a session shows, and its size in the picture. */
export interface ShownWindow extends WindowInfo {
  width: number
  height: number
}

export interface HostInfo {
  name: string
  version: string
  protocol: number
  os: string
  displays: DisplayInfo[]
  /** Devices controlling this PC right now. */
  controllers?: string[]
  /**
   * What another PC on the same network needs to wake this one. Missing
   * before 0.1.8, which also can't sleep, restart or shut down when asked.
   */
  wake?: WakeInfo
}

export interface WakeInfo {
  /** Wired and Wi-Fi adapters with an address on a local network. */
  adapters: WakeAdapter[]
  /** A test wake has worked, so waking is offered while it's asleep. */
  verified: boolean
}

export interface WakeAdapter {
  mac: string
  /** IPv4 address on the local network. */
  address: string
  prefix: number
}

export type PowerAction = 'sleep' | 'restart' | 'shutDown'

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
  /** Tailscale says it's online, but it hasn't answered over the tailnet lately, so it's likely asleep. */
  silent?: boolean
  /** How to wake it, remembered from when it was last online. */
  wake?: WakeInfo
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
  | { state: 'failed'; message: string; installing: boolean }

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
  /** Show only this window. Hosts from 0.1.10. */
  | { t: 'selectWindow'; id: string }
  | { t: 'listWindows' }
  | { t: 'requestKeyframe' }
  | { t: 'setPaused'; paused: boolean }
  | { t: 'clipboard'; text: string }
  | { t: 'setAudio'; enabled: boolean }
  /** Cover the remote PC's displays so nobody there sees the session. Ends with it. */
  | { t: 'hideScreen'; hide: boolean }

export type HostMsg =
  | { t: 'hello'; host: HostInfo; display: string }
  | { t: 'displays'; displays: DisplayInfo[]; active: string; window?: ShownWindow }
  /** The windows open on the remote PC, frontmost first. */
  | { t: 'windows'; windows: WindowInfo[] }
  | { t: 'stats'; bitrateKbps: number; fps: number; width: number; height: number; encoder: string }
  | { t: 'error'; message: string }
  | { t: 'clipboard'; text: string }
  | { t: 'cursor'; name: string }
  | { t: 'route'; route: Route }
  /** Files copied on the host, fetched by `offer`. Empty when the clipboard no longer holds files. */
  | { t: 'filesCopied'; offer: number; files: CopiedFile[]; folders: number }

/** How the connection travels over the tailnet. `region` names one of Tailscale's relay server regions. */
export type Route = { kind: 'direct' } | { kind: 'peerRelay' } | { kind: 'relay'; region: string }

export interface CopiedFile {
  name: string
  size: number
}

/** The host's answer on a file channel. */
export type FileReply = { t: 'saved'; name: string } | { t: 'failed'; message: string }
