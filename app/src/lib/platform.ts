import { invoke, type InvokeArgs, type InvokeOptions } from '@tauri-apps/api/core'
import { devices as fetchDevices } from './api'
import { toProblem } from './errors'
import type { FileSink } from './files'
import type { ControlStatus, Device } from './types'

/** Running inside the Conductor app rather than a browser. */
export const isApp = '__TAURI_INTERNALS__' in window
/** The Windows app, which can read the tailnet and host this PC. */
export const isDesktopApp = isApp && /Windows/.test(navigator.userAgent)

async function call<T>(cmd: string, args?: InvokeArgs, options?: InvokeOptions): Promise<T> {
  try {
    return await invoke<T>(cmd, args, options)
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

/**
 * Somewhere to save a file from the other PC: straight into the Downloads
 * folder in the app, or a download once it has all arrived in browsers.
 */
export async function saveFile(name: string): Promise<FileSink> {
  if (isDesktopApp) {
    const id = await call<number>('begin_save', { name })
    return {
      write: (data) => call('write_save', data, { headers: { 'save-id': String(id) } }),
      finish: () => call('end_save', { id }),
      abort: () => call('abort_save', { id }),
    }
  }
  const parts: ArrayBuffer[] = []
  return {
    write: async (data) => void parts.push(data),
    finish: async () => {
      const url = URL.createObjectURL(new Blob(parts))
      const link = document.createElement('a')
      link.href = url
      link.download = name
      document.body.append(link)
      link.click()
      link.remove()
      // The download has its own copy long before then.
      setTimeout(() => URL.revokeObjectURL(url), 60_000)
      return name
    },
    abort: async () => void parts.splice(0),
  }
}
