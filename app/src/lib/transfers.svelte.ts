import { Problem, toProblem } from './errors'
import { COPIED_CHANNEL, FILE_CHANNEL, HostError, receiveFile, sendFile, type FileSink, type Sendable } from './files'
import { canCopyImages, isPhoneApp, saveFile, savesToDownloads, writeClipboardImage } from './platform'
import type { ClientMsg, CopiedFile } from './types'

/** Pictures copied on the remote PC up to this size go straight onto this device's clipboard. */
const COPY_IMAGE_MAX = 16 * 1024 * 1024
/** Something pasted that takes longer than this to get there isn't pasted, only put on the clipboard, as the user may have moved on. */
const PASTE_KEYS_WITHIN_MS = 3000

interface Progress {
  files: number
  /** The file moving now, counting from 1. */
  current: number
  /** Bytes in all the files, and how many of them have moved. */
  total: number
  done: number
  /** The file moving now is being pasted, rather than sent to Downloads. */
  pasting: boolean
}

/** Files or a picture on their way to be pasted on the remote PC. */
interface Paste {
  id: number
  image: boolean
  started: number
  /** Files not there yet. */
  left: number
}

interface Queued {
  file: Sendable
  paste?: Paste
}

interface Failure {
  chip: string
  problem: Problem
}

/**
 * Files sent to the Downloads folder on the remote PC or pasted there, and
 * files and pictures copied there brought to this device. Each way moves one
 * file at a time.
 */
export class Transfers {
  sending = $state<Progress | null>(null)
  saving = $state<Progress | null>(null)
  /** Files or a picture copied on the remote PC, which can be saved here. */
  copied = $state<{ offer: number; files: CopiedFile[]; image: boolean } | null>(null)
  /** Why the last transfer failed, explained in the session menu. */
  failure = $state<Failure | null>(null)
  /** Shown for a moment, such as how the last transfer ended. */
  note = $state<string | null>(null)

  #openChannel: (label: string) => RTCDataChannel | null
  #pc: () => string
  #send: (msg: ClientMsg) => void
  #queue: Queued[] = []
  // Folders dropped or pasted with the files, which can't be sent.
  #skippedFolders = 0
  #stopSending: AbortController | null = null
  #stopSaving: AbortController | null = null
  #noteTimer: number | undefined
  #lastPaste = 0
  // Pastes the remote PC hasn't answered yet, and whether it presses the paste keys for each.
  #pastes = new Map<number, boolean>()
  // The latest picture copied on the remote PC, while it's brought here, and once it's on this device's clipboard.
  #imageOffer = 0
  #imageCopied = 0
  #stopImage: AbortController | null = null

  constructor(
    openChannel: (label: string) => RTCDataChannel | null,
    pc: () => string,
    send: (msg: ClientMsg) => void,
  ) {
    this.#openChannel = openChannel
    this.#pc = pc
    this.#send = send
  }

  /** What the session handle says about files, most important first. */
  get notice() {
    if (this.failure) return { ...this.failure, warning: true }
    if (this.sending) return progressNotice(this.sending.pasting ? 'Pasting' : 'Sending', this.sending)
    if (this.saving) return progressNotice('Saving', this.saving)
    if (this.note) return { chip: this.note }
    return null
  }

  /** The picture copied on the remote PC is on this device's clipboard too. */
  get imageCopiedHere() {
    return !!this.copied?.image && this.copied.offer === this.#imageCopied
  }

  /** Sends files to the Downloads folder on the remote PC, after any already on their way. */
  send(files: Sendable[], folders = 0) {
    this.#clear()
    this.#skippedFolders += folders
    if (!files.length) {
      if (!this.sending && this.#skippedFolders) {
        this.failure = foldersNotSent()
        this.#skippedFolders = 0
      }
      return
    }
    this.#enqueue(files.map((file) => ({ file })))
  }

  /**
   * Puts files or a picture on the remote PC's clipboard, after anything
   * already on its way, and pastes them there if they get there quickly.
   */
  paste(files: Sendable[], folders: number, image: boolean) {
    this.#clear()
    if (!files.length) {
      if (folders) this.failure = foldersNotPasted()
      return
    }
    this.#skippedFolders += folders
    const paste: Paste = { id: ++this.#lastPaste, image, started: performance.now(), left: files.length }
    this.#enqueue(files.map((file) => ({ file, paste })))
  }

  #enqueue(items: Queued[]) {
    this.#queue.push(...items)
    const size = items.reduce((n, i) => n + i.file.size, 0)
    if (this.sending) {
      this.sending.files += items.length
      this.sending.total += size
    } else {
      this.sending = { files: items.length, current: 1, total: size, done: 0, pasting: !!items[0].paste }
      void this.#sendQueue()
    }
  }

  stopSending() {
    this.#stopSending?.abort()
  }

  /** The remote PC put what was pasted on its clipboard, or `message` says why it couldn't. */
  hostPasted(id: number, message?: string) {
    const keys = this.#pastes.get(id)
    this.#pastes.delete(id)
    if (message) {
      this.failure = {
        chip: 'Couldn’t paste',
        problem: new Problem(`Couldn’t put what you pasted on the clipboard of ${this.#pc()}. Try pasting again.`, message),
      }
    } else if (keys === false && !this.failure) {
      this.#showNote(`Copied to ${this.#pc()}. Paste there with Ctrl+V`)
    }
  }

  /** Files or a picture were copied on the remote PC, or what was copied there no longer is. */
  hostCopied(offer: number, files: CopiedFile[], folders: number, image = false) {
    this.copied = files.length ? { offer, files, image } : null
    this.#stopImage?.abort()
    if (image && files.length === 1) {
      if (canCopyImages && files[0].size <= COPY_IMAGE_MAX) void this.#copyImage(offer)
      else this.#showNote('Image copied. Save it from the menu')
      return
    }
    const copied = files.length === 1 ? 'File copied' : `${files.length} files copied`
    if (files.length && folders) this.#showNote(`${copied}. Folders can’t be saved`)
    else if (files.length) this.#showNote(`${copied}. Save ${files.length === 1 ? 'it' : 'them'} from the menu`)
    else if (folders) this.#showNote('Copied folders can’t be saved. Zip them first')
  }

  /** Saves the files copied on the remote PC on this device. */
  save() {
    if (!this.copied || this.saving) return
    this.#clear()
    void this.#saveCopied(this.copied.offer, this.copied.files)
  }

  stopSaving() {
    this.#stopSaving?.abort()
  }

  /** Stops everything, for when the session ends. */
  stop() {
    this.#dropQueue()
    this.stopSending()
    this.stopSaving()
    this.#stopImage?.abort()
    clearTimeout(this.#noteTimer)
  }

  async #sendQueue() {
    const stop = new AbortController()
    this.#stopSending = stop
    // Bytes of the files already sent, and how many there were, and how many went to Downloads.
    let before = 0
    let count = 0
    let sent = 0
    let item: Queued | undefined
    try {
      while ((item = this.#queue.shift())) {
        const { file, paste } = item
        if (this.sending) {
          this.sending.current = count + 1
          this.sending.pasting = !!paste
        }
        try {
          const channel = this.#openChannel(FILE_CHANNEL)
          if (!channel) throw new Problem(`Couldn’t send ${file.name}. ${this.#pc()} isn’t connected right now.`)
          const start = before
          const progress = (bytes: number) => this.sending && (this.sending.done = start + bytes)
          await sendFile(channel, file, progress, stop.signal, paste?.id)
        } finally {
          file.close?.()
        }
        before += file.size
        count++
        if (!paste) sent++
        else if (--paste.left === 0) {
          const keys = performance.now() - paste.started < PASTE_KEYS_WITHIN_MS
          this.#pastes.set(paste.id, keys)
          this.#send({ t: 'paste', id: paste.id, image: paste.image, keys })
        }
      }
      if (this.#skippedFolders) {
        this.failure = sent ? { ...foldersNotSent(), chip: 'Sent to Downloads. Folders weren’t sent' } : foldersNotPasted()
      } else if (sent) {
        this.#showNote('Sent to Downloads')
      }
    } catch (e) {
      const rest = this.#queue.length
      const total = count + 1 + rest
      this.#dropQueue()
      if (stop.signal.aborted) {
        this.#showNote(stopped(count, total, 'sending', 'sent'))
      } else if (item) {
        const { file, paste } = item
        const after = notAfter(rest, 'sent')
        const again = file.shared
          ? `To try again, share ${rest ? 'the files' : 'it'} again.`
          : paste
            ? 'Try pasting again.'
            : 'Try sending again.'
        const what = paste?.image ? 'the picture' : file.name
        this.failure = {
          chip: paste ? 'Couldn’t paste' : `Couldn’t send ${file.name}`,
          problem:
            e instanceof HostError
              ? new Problem(`Couldn’t save ${what} on ${this.#pc()}.${after} ${again}`, e.message)
              : toProblem(e, `Couldn’t send ${what} to ${this.#pc()}.${after} ${again}`),
        }
      }
    } finally {
      this.sending = null
      this.#stopSending = null
      this.#skippedFolders = 0
    }
  }

  // Brings a picture copied on the remote PC onto this device's clipboard,
  // or offers to save it when it can't go there.
  async #copyImage(offer: number) {
    const stop = new AbortController()
    this.#stopImage = stop
    this.#imageOffer = offer
    const parts: ArrayBuffer[] = []
    const memory: FileSink = {
      write: async (data) => void parts.push(data),
      finish: async () => '',
      abort: async () => void parts.splice(0),
    }
    try {
      const channel = this.#openChannel(COPIED_CHANNEL)
      if (!channel) throw new Error('Not connected.')
      await receiveFile(channel, { offer, index: 0 }, async () => memory, () => {}, stop.signal)
      if (!(await writeClipboardImage(new Blob(parts, { type: 'image/png' })))) throw new Error('Not copied.')
      this.#imageCopied = offer
    } catch {
      // Unless something newer was copied meanwhile.
      if (!stop.signal.aborted && this.#imageOffer === offer) this.#showNote('Image copied. Save it from the menu')
    } finally {
      if (this.#stopImage === stop) this.#stopImage = null
    }
  }

  async #saveCopied(offer: number, files: CopiedFile[]) {
    const stop = new AbortController()
    this.#stopSaving = stop
    this.saving = { files: files.length, current: 1, total: files.reduce((n, f) => n + f.size, 0), done: 0, pasting: false }
    // Bytes of the files already saved, and how many there were.
    let before = 0
    let index = 0
    try {
      for (; index < files.length; index++) {
        const { name, size } = files[index]
        if (this.saving) this.saving.current = index + 1
        const channel = this.#openChannel(COPIED_CHANNEL)
        if (!channel) throw new Problem(`Couldn’t save ${name}. ${this.#pc()} isn’t connected right now.`)
        const start = before
        const progress = (bytes: number) => this.saving && (this.saving.done = start + bytes)
        await receiveFile(channel, { offer, index }, saveFile, progress, stop.signal)
        before += size
      }
      this.#showNote(savesToDownloads ? 'Saved to Downloads' : isPhoneApp ? 'Saved in Conductor’s folder' : 'Downloaded')
      // Saved; copying them again offers them again.
      if (this.copied?.offer === offer) this.copied = null
    } catch (e) {
      if (stop.signal.aborted) {
        this.#showNote(stopped(index, files.length, 'saving', 'saved'))
      } else {
        const name = files[index]?.name ?? 'a file'
        const after = notAfter(files.length - index - 1, 'saved')
        this.failure = {
          chip: `Couldn’t save ${name}`,
          problem:
            e instanceof HostError
              ? new Problem(`Couldn’t read ${name} on ${this.#pc()}.${after} Check it’s still there, then copy it again.`, e.message)
              : toProblem(e, `Couldn’t save ${name} from ${this.#pc()}.${after} Try saving again.`),
        }
      }
    } finally {
      this.saving = null
      this.#stopSaving = null
    }
  }

  #dropQueue() {
    for (const { file } of this.#queue) file.close?.()
    this.#queue = []
  }

  // A new transfer replaces whatever the handle said about the last one.
  #clear() {
    clearTimeout(this.#noteTimer)
    this.note = null
    this.failure = null
  }

  #showNote(text: string) {
    clearTimeout(this.#noteTimer)
    this.note = text
    this.#noteTimer = window.setTimeout(() => (this.note = null), 4000)
  }
}

function progressNotice(verb: string, p: Progress) {
  return {
    chip: p.files === 1 ? (p.pasting ? 'Pasting…' : `${verb} file…`) : `${verb} file ${p.current} of ${p.files}…`,
    progress: p.total ? Math.floor((p.done / p.total) * 100) : 0,
  }
}

// How a stopped transfer ended: `done` of `files` had arrived.
function stopped(done: number, files: number, doing: string, did: string) {
  if (files === 1) return `Stopped ${doing}`
  return done ? `Stopped. ${capitalize(did)} ${done} of ${files} files` : `Stopped. Nothing was ${did}`
}

// The files after a failed one, which weren't tried.
function notAfter(rest: number, did: string) {
  if (!rest) return ''
  return rest === 1 ? ` The file after it wasn’t ${did} either.` : ` The ${rest} files after it weren’t ${did} either.`
}

const capitalize = (s: string) => s[0].toUpperCase() + s.slice(1)

const foldersNotSent = (): Failure => ({
  chip: 'Folders weren’t sent',
  problem: new Problem('To send a folder, zip it first or send the files in it.'),
})

const foldersNotPasted = (): Failure => ({
  chip: 'Folders weren’t pasted',
  problem: new Problem('To paste a folder, zip it first or copy the files in it.'),
})
