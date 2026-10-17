import { android, SharedFile } from './android'

interface Waiting {
  files: { id: number; name: string; size: number }[]
  text: boolean
  unreadable: number
}

/** What was shared to the Android app from other apps, waiting for a PC to send it to. */
class Share {
  files = $state.raw<SharedFile[]>([])
  /** Only text was shared, which can't be sent. */
  text = $state(false)
  /** Shared files that couldn't be read. */
  unreadable = $state(0)

  constructor() {
    if (!android) return
    this.#refresh()
    // Shared while the app is open. Listeners added later hear about it after this one.
    window.addEventListener('conductorshare', () => this.#refresh())
  }

  get any() {
    return this.files.length > 0 || this.text || this.unreadable > 0
  }

  /** Takes the files, to send them now. */
  take(): SharedFile[] {
    const files = this.files
    android?.taken()
    this.#reset()
    return files
  }

  /** Doesn't send them. */
  dismiss() {
    android?.clearShared()
    this.#reset()
  }

  #refresh() {
    try {
      const waiting = JSON.parse(android!.shared()) as Waiting
      this.files = waiting.files.map((f) => new SharedFile(f.id, f.name, f.size))
      this.text = waiting.text
      this.unreadable = waiting.unreadable
    } catch {
      this.#reset()
    }
  }

  #reset() {
    this.files = []
    this.text = false
    this.unreadable = 0
  }
}

export const share = new Share()
