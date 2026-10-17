import type { Device, HostInfo } from './types'

/** Base URL of a host's API. Empty string means the host serving this page. */
export type HostUrl = string

async function request<T>(url: string, init?: RequestInit): Promise<T> {
  let res: Response
  try {
    res = await fetch(url, { ...init, signal: init?.signal ?? AbortSignal.timeout(8000) })
  } catch {
    throw new Error("Couldn't reach this PC. It may be off, asleep, or not on your tailnet.")
  }
  if (res.status === 403) throw new Error("This device isn't allowed to connect to that PC.")
  if (!res.ok) throw new Error((await res.text()) || `Request failed (${res.status})`)
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
