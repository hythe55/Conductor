import { invoke, type InvokeArgs, type InvokeOptions } from '@tauri-apps/api/core'
import { android, toBase64 } from './android'
import { devices as fetchDevices, PORT, Unreachable } from './api'
import { Problem, toProblem } from './errors'
import type { FileSink } from './files'
import type { ControlStatus, Device } from './types'

/** Running inside the Conductor app rather than a browser. */
export const isApp = '__TAURI_INTERNALS__' in window
/** The Windows app, which can read the tailnet and host this PC. */
export const isDesktopApp = isApp && /Windows/.test(navigator.userAgent)
/** The phone app. Phones can't read the tailnet, so it asks one of the owner's PCs. */
export const isPhoneApp = isApp && !isDesktopApp

const PCS_KEY = 'conductor.pcs'

/** PCs the phone app can ask for the device list, the one that answered last first. */
export function knownPcs(): string[] {
  try {
    const pcs = JSON.parse(localStorage.getItem(PCS_KEY) ?? '[]')
    return Array.isArray(pcs) ? pcs.filter((p) => typeof p === 'string') : []
  } catch {
    return []
  }
}

function rememberPcs(pcs: string[]) {
  try {
    localStorage.setItem(PCS_KEY, JSON.stringify([...new Set(pcs)]))
  } catch {
    // Asked for again next time.
  }
}

/** Remembers `pc`, which just answered with `list`, and the other PCs on it, for next time. */
function rememberAnswer(pc: string, list: Device[]) {
  const others = list.filter((d) => d.host && !d.isYou).map((d) => d.address)
  rememberPcs([pc, ...others, ...knownPcs()].slice(0, 16))
}

/** Asks `pc` (a name or tailnet address) for the device list. */
export async function devicesFrom(pc: string): Promise<Device[]> {
  const list = await fetchDevices(`http://${pc}:${PORT}`)
  rememberAnswer(pc, list)
  return list
}

async function devicesFromKnownPcs(): Promise<Device[]> {
  const pcs = knownPcs()
  if (pcs.length === 0) throw new Problem('Enter the name of a PC that runs Conductor.')
  // All at once, since a PC that's asleep takes seconds to give up on.
  const asks = pcs.map(async (pc) => ({ pc, list: await fetchDevices(`http://${pc}:${PORT}`) }))
  try {
    const { pc, list } = await Promise.any(asks)
    rememberAnswer(pc, list)
    return list
  } catch (e) {
    const errors: unknown[] = e instanceof AggregateError ? e.errors : [e]
    const other = errors.find((x) => !(x instanceof Unreachable))
    if (other) throw toProblem(other, 'Couldn’t get the list of devices.')
    throw new Problem(
      pcs.length === 1
        ? `Couldn’t reach ${pcs[0]}. It may be off, asleep, or not on your tailnet.`
        : 'Couldn’t reach any of your PCs. They may be off, asleep, or not on your tailnet.',
      errors.map((x) => (x as Unreachable).details).join('\n'),
    )
  }
}

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
  if (isPhoneApp) return devicesFromKnownPcs()
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

export function setPreviews(enabled: boolean): Promise<ControlStatus> {
  return call('set_previews', { enabled })
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

/** Whether pictures can be put on this device's clipboard. Browsers only allow it on https pages. */
export const canCopyImages =
  isDesktopApp || (window.isSecureContext && typeof ClipboardItem !== 'undefined' && !!navigator.clipboard?.write)

// The clipboard's sequence number once Conductor last put a picture there, in the desktop app.
let imageSequence: number | null = null

/** Puts a PNG on this device's clipboard. Browsers only allow it while the page has focus. */
export async function writeClipboardImage(png: Blob): Promise<boolean> {
  try {
    if (isDesktopApp) {
      imageSequence = await invoke<number>('write_clipboard_image', new Uint8Array(await png.arrayBuffer()))
    } else {
      await navigator.clipboard.write([new ClipboardItem({ 'image/png': png })])
    }
    return true
  } catch {
    return false
  }
}

/** Nothing has been copied on this device since Conductor last put a picture there. Only known in the desktop app. */
export async function stillHoldsImage(): Promise<boolean> {
  if (!isDesktopApp || imageSequence == null) return false
  return invoke<number>('clipboard_sequence').then(
    (sequence) => sequence === imageSequence,
    () => false,
  )
}

/** Whether saved files go straight into the Downloads folder, rather than being downloaded. */
export const savesToDownloads = isDesktopApp || (android?.savesToDownloads() ?? false)

/**
 * Somewhere to save a file from the other PC: straight into the Downloads
 * folder in the apps, or a download once it has all arrived in browsers.
 */
export async function saveFile(name: string): Promise<FileSink> {
  if (android) {
    const files = android
    const id = files.beginSave(name)
    if (id < 0) throw new Error('Android didn’t let Conductor create the file.')
    return {
      write: async (data) => {
        if (!files.writeSave(id, toBase64(data))) throw new Error('Android didn’t let Conductor write the file.')
      },
      finish: async () => {
        const saved = files.finishSave(id)
        if (!saved) throw new Error('Android didn’t let Conductor finish the file.')
        return saved
      },
      abort: async () => files.abortSave(id),
    }
  }
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
