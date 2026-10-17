import { invoke } from '@tauri-apps/api/core'
import { devices as fetchDevices } from './api'
import { toProblem } from './errors'
import type { ControlStatus, Device } from './types'

/** Running inside the Conductor app rather than a browser. */
export const isApp = '__TAURI_INTERNALS__' in window
/** The Windows app, which can read the tailnet and host this PC. */
export const isDesktopApp = isApp && /Windows/.test(navigator.userAgent)

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args)
  } catch (e) {
    throw toProblem(e, 'Something went wrong inside Conductor. Restart the app and try again.')
  }
}

/** The owner's PCs. The device this runs on is marked `isYou`. */
export function listDevices(): Promise<Device[]> {
  if (isDesktopApp) return call('devices')
  return fetchDevices('')
}

/** Hosting state of this PC, or null when it can't host (browser, phone, no service). */
export function hostStatus(): Promise<ControlStatus | null> {
  if (!isDesktopApp) return Promise.resolve(null)
  return call('host_status')
}

export function setHosting(enabled: boolean): Promise<ControlStatus> {
  return call('set_hosting', { enabled })
}

export function setAutoUpdate(enabled: boolean): Promise<ControlStatus> {
  return call('set_auto_update', { enabled })
}

export function checkForUpdates(): Promise<ControlStatus> {
  return call('check_for_updates')
}

/** Closes the app while the update installs; the service opens it again afterwards. */
export function installUpdate(): Promise<ControlStatus> {
  return call('install_update')
}

/** Text on this device's clipboard, where it can be read without asking. Null in browsers. */
export function readClipboard(): Promise<string | null> {
  if (!isDesktopApp) return Promise.resolve(null)
  return invoke<string | null>('read_clipboard').catch(() => null)
}

/** Puts text on this device's clipboard. Browsers only allow it while the page has focus. */
export async function writeClipboard(text: string): Promise<boolean> {
  if (isDesktopApp) return invoke('write_clipboard', { text }).then(
    () => true,
    () => false,
  )
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    // Plain-http pages have no clipboard API, but a copy command works shortly after a key press.
    let copied = false
    const onCopy = (e: ClipboardEvent) => {
      e.clipboardData?.setData('text/plain', text)
      e.preventDefault()
      copied = true
    }
    document.addEventListener('copy', onCopy)
    try {
      document.execCommand('copy')
    } catch {
      // Not allowed right now.
    }
    document.removeEventListener('copy', onCopy)
    return copied
  }
}
