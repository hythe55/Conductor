import { Problem, toProblem } from './errors'
import { COPIED_CHANNEL, FILE_CHANNEL, HostError, receiveFile, sendFile, type Sendable } from './files'
import { isPhoneApp, saveFile, savesToDownloads } from './platform'
import type { CopiedFile } from './types'

interface Progress {
  files: number
  /** The file moving now, counting from 1. */
  current: number
  /** Bytes in all the files, and how many of them have moved. */
  total: number
  done: number
}

interface Failure {
  chip: string
  problem: Problem
}

/**
 * Files sent to the Downloads folder on the remote PC, and files copied there
 * saved on this device. Each way moves one file at a time.
 */
export class Transfers {
  sending = $state<Progress | null>(null)
  saving = $state<Progress | null>(null)
  /** Files copied on the remote PC, which can be saved here. */
  copied = $state<{ offer: number; files: CopiedFile[] } | null>(null)
  /** Why the last transfer failed, explained in the session menu. */
  failure = $state<Failure | null>(null)
  /** Shown for a moment, such as how the last transfer ended. */
  note = $state<string | null>(null)

  #openChannel: (label: string) => RTCDataChannel | null
  #pc: () => string
  #queue: Sendable[] = []
  // Folders dropped with the files, which can't be sent.
  #skippedFolders = 0
  #stopSending: AbortController | null = null
  #stopSaving: AbortController | null = null
  #noteTimer: number | undefined

  constructor(openChannel: (label: string) => RTCDataChannel | null, pc: () => string) {
    this.#openChannel = openChannel
    this.#pc = pc
  }

  /** What the session handle says about files, most important first. */
  get notice() {
    if (this.failure) return { ...this.failure, warning: true }
    if (this.sending) return progressNotice('Sending', this.sending)
    if (this.saving) return progressNotice('Saving', this.saving)
    if (this.note) return { chip: this.note }
    return null
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
    this.#queue.push(...files)
    const size = files.reduce((n, f) => n + f.size, 0)
    if (this.sending) {
      this.sending.files += files.length
      this.sending.total += size
    } else {
      this.sending = { files: files.length, current: 1, total: size, done: 0 }
      void this.#sendQueue()
    }
  }

  stopSending() {
    this.#stopSending?.abort()
  }

  /** Files were copied on the remote PC, or the files copied there no longer are. */
  hostCopied(offer: number, files: CopiedFile[], folders: number) {
    this.copied = files.length ? { offer, files } : null
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
    clearTimeout(this.#noteTimer)
  }

  async #sendQueue() {
    const stop = new AbortController()
    this.#stopSending = stop
    // Bytes of the files already sent, and how many there were.
    let before = 0
    let count = 0
    let file: Sendable | undefined
    try {
      while ((file = this.#queue.shift())) {
        if (this.sending) this.sending.current = count + 1
        try {
          const channel = this.#openChannel(FILE_CHANNEL)
          if (!channel) throw new Problem(`Couldn’t send ${file.name}. ${this.#pc()} isn’t connected right now.`)
          const start = before
          await sendFile(channel, file, (bytes) => this.sending && (this.sending.done = start + bytes), stop.signal)
        } finally {
          file.close?.()
        }
        before += file.size
        count++
      }
      if (this.#skippedFolders) this.failure = { ...foldersNotSent(), chip: 'Sent to Downloads. Folders weren’t sent' }
      else this.#showNote('Sent to Downloads')
    } catch (e) {
      const rest = this.#queue.length
      const total = count + 1 + rest
      this.#dropQueue()
      if (stop.signal.aborted) {
        this.#showNote(stopped(count, total, 'sending', 'sent'))
      } else if (file) {
        const after = notAfter(rest, 'sent')
        const again = file.shared ? `To try again, share ${rest ? 'the files' : 'it'} again.` : 'Try sending again.'
        this.failure = {
          chip: `Couldn’t send ${file.name}`,
          problem:
            e instanceof HostError
              ? new Problem(`Couldn’t save ${file.name} on ${this.#pc()}.${after} ${again}`, e.message)
              : toProblem(e, `Couldn’t send ${file.name} to ${this.#pc()}.${after} ${again}`),
        }
      }
    } finally {
      this.sending = null
      this.#stopSending = null
      this.#skippedFolders = 0
    }
  }

  async #saveCopied(offer: number, files: CopiedFile[]) {
    const stop = new AbortController()
    this.#stopSaving = stop
    this.saving = { files: files.length, current: 1, total: files.reduce((n, f) => n + f.size, 0), done: 0 }
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
    for (const file of this.#queue) file.close?.()
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
    chip: p.files === 1 ? `${verb} file…` : `${verb} file ${p.current} of ${p.files}…`,
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
