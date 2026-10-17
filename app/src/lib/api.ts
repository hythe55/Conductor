import { Problem } from './errors'
import type { Device, HostInfo, PowerAction, PowerSchedule, WakeAdapter, WakeInfo } from './types'

/** The PC didn't answer at all. */
export class Unreachable extends Problem {}

/** Base URL of a host's API. Empty string means the host serving this page. */
export type HostUrl = string

async function request<T>(url: string, init?: RequestInit): Promise<T> {
  let res: Response
  try {
    res = await fetch(url, { ...init, signal: init?.signal ?? AbortSignal.timeout(8000) })
  } catch (e) {
    throw new Unreachable(
      "Couldn't reach this PC. It may be off, asleep, or not on your tailnet.",
      e instanceof Error ? e.message : String(e),
    )
  }
  const detail = async () => `${res.status} ${res.statusText}: ${(await res.text().catch(() => '')).trim()}`
  if (res.status === 403) {
    throw new Problem(
      "This device isn't allowed to control that PC. Both need to be signed in to the same Tailscale account.",
      await detail(),
    )
  }
  if (!res.ok) throw new Problem("That PC couldn't handle the request. Try again, or restart it.", await detail())
  if (res.status === 204) return undefined as T
  return res.json() as Promise<T>
}

export function hostInfo(host: HostUrl): Promise<HostInfo> {
  return request(`${host}/api/info`)
}

export function devices(host: HostUrl): Promise<Device[]> {
  return request(`${host}/api/devices`)
}

export const PORT = 4280

/** Where to reach a device's API from this page. */
export function deviceUrl(d: Device): HostUrl {
  // The host serving this page (or the dev server's proxy to it) is reachable
  // at the page's own origin. The desktop app's origin has no port.
  if (d.isSelf && location.port !== '') return ''
  return `http://${d.address}:${PORT}`
}

export interface SessionOptions {
  display?: string
  maxWidth?: number
  maxHeight?: number
}

export async function createSession(host: HostUrl, sdp: string, opts: SessionOptions): Promise<string> {
  const res = await request<{ sdp: string }>(`${host}/api/session`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ sdp, ...opts }),
  })
  return res.sdp
}

function post<T>(url: string, body: unknown): Promise<T> {
  return request(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
}

/** Whether the host answers right now. */
export async function reachable(host: HostUrl): Promise<boolean> {
  try {
    await request(`${host}/api/info`, { signal: AbortSignal.timeout(3000) })
    return true
  } catch {
    return false
  }
}

export function power(host: HostUrl, action: PowerAction): Promise<void> {
  return post(`${host}/api/power`, { action })
}

/** Replaces the PC's schedule, and returns it as saved. */
export function setSchedule(host: HostUrl, schedule: PowerSchedule[]): Promise<PowerSchedule[]> {
  return post(`${host}/api/schedule`, { schedule })
}

/** Asks `host` to send wake packets to another PC on its network. */
export function wake(host: HostUrl, adapters: WakeAdapter[]): Promise<void> {
  return post(`${host}/api/wake`, { adapters })
}

/** Tells a PC that just woke from a test that waking it works. */
export function confirmWake(host: HostUrl): Promise<WakeInfo> {
  return post(`${host}/api/wake/confirm`, {})
}
