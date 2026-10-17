import { Problem } from './errors'
import type { Device, HostInfo } from './types'

/** Base URL of a host's API. Empty string means the host serving this page. */
export type HostUrl = string

async function request<T>(url: string, init?: RequestInit): Promise<T> {
  let res: Response
  try {
    res = await fetch(url, { ...init, signal: init?.signal ?? AbortSignal.timeout(8000) })
  } catch (e) {
    throw new Problem(
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
  return res.json() as Promise<T>
}

export function hostInfo(host: HostUrl): Promise<HostInfo> {
  return request(`${host}/api/info`)
}

export function devices(host: HostUrl): Promise<Device[]> {
  return request(`${host}/api/devices`)
}

const PORT = 4280

/** Where to reach a device's API from this page. */
export function deviceUrl(d: Device): HostUrl {
  // The host serving this page is reachable at the page's own origin.
  if (d.isSelf && location.port === String(PORT)) return ''
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
