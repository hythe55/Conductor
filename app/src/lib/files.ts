import type { CopiedFile, FileReply } from './types'

// Each file goes on a data channel of its own: its name and size, then its
// bytes. When sending, the host answers once it's saved, and closing the
// channel early makes it delete what arrived. When fetching a file copied on
// the host, the client asks for it and the host sends it the same way.

export const FILE_CHANNEL = 'file'
export const COPIED_CHANNEL = 'copied'

const CHUNK = 64 * 1024
// Reading ahead of the network only fills memory, so wait while this much is queued.
const QUEUED_MAX = 1024 * 1024
// Bytes gathered before each write when saving, so the app isn't called for every message.
const WRITE_SIZE = 1024 * 1024

/** The host couldn't save or read the file. */
export class HostError extends Error {}

/** A file to send: one picked or dropped, or one shared to the Android app. */
export interface Sendable {
  readonly name: string
  readonly size: number
  slice(start: number, end: number): { arrayBuffer(): Promise<ArrayBuffer> }
  /** Shared from another app, so sending it again means sharing it again. */
  readonly shared?: boolean
  /** Done with it, sent or not. */
  close?(): void
}

/** Where a file from the other PC is written as it arrives. */
export interface FileSink {
  write(data: ArrayBuffer): Promise<void>
  /** Resolves with the name the file was saved under. */
  finish(): Promise<string>
  /** Throws away what was written. */
  abort(): Promise<void>
}

/**
 * Sends one file. Resolves with the name the host saved it under. `progress`
 * is called with the bytes that have left this device so far.
 */
export async function sendFile(
  channel: RTCDataChannel,
  file: Sendable,
  progress: (bytes: number) => void,
  signal: AbortSignal,
): Promise<string> {
  if (signal.aborted) {
    channel.close()
    throw signal.reason
  }
  let read = 0
  const report = () => progress(Math.max(0, read - channel.bufferedAmount))
  const timer = window.setInterval(report, 250)
  channel.binaryType = 'arraybuffer'
  channel.bufferedAmountLowThreshold = QUEUED_MAX / 4

  // Settles on the host's answer, the channel closing, or the user stopping it.
  const answer = new Promise<string>((resolve, reject) => {
    channel.onmessage = (e) => {
      try {
        const reply = JSON.parse(e.data as string) as FileReply
        if (reply.t === 'saved') resolve(reply.name)
        else reject(new HostError(reply.message))
      } catch {
        reject(new Error('The answer from the other PC didn’t make sense.'))
      }
    }
    channel.onclose = () => reject(new Error('The connection closed before the file arrived.'))
    signal.addEventListener('abort', () => reject(signal.reason), { once: true })
  })
  // Waits for `ready`, unless the transfer fails or is stopped first.
  const failed = answer.then(() => new Promise<never>(() => {}))
  failed.catch(() => {})
  const until = <T>(ready: Promise<T>) => Promise.race([ready, failed])
  const once = (type: string) => until(new Promise<void>((r) => channel.addEventListener(type, () => r(), { once: true })))

  try {
    if (channel.readyState !== 'open') await once('open')
    channel.send(JSON.stringify({ name: file.name, size: file.size }))
    while (read < file.size) {
      if (channel.bufferedAmount > QUEUED_MAX) await once('bufferedamountlow')
      const chunk = await until(file.slice(read, read + CHUNK).arrayBuffer())
      channel.send(chunk)
      read += chunk.byteLength
    }
    return await answer
  } finally {
    clearInterval(timer)
    report()
    channel.onmessage = null
    channel.onclose = null
    channel.close()
  }
}

/**
 * Fetches one of the files copied on the host. `open` is given the file's
 * name once the host sends it. Resolves with the name it was saved under.
 * `progress` is called with the bytes that have arrived so far.
 */
export async function receiveFile(
  channel: RTCDataChannel,
  request: { offer: number; index: number },
  open: (name: string) => Promise<FileSink>,
  progress: (bytes: number) => void,
  signal: AbortSignal,
): Promise<string> {
  if (signal.aborted) {
    channel.close()
    throw signal.reason
  }
  let received = 0
  const timer = window.setInterval(() => progress(received), 250)
  channel.binaryType = 'arraybuffer'
  const file: { sink?: FileSink } = {}
  // Opening the file and each write happen in order, each after the one before.
  let saving: Promise<unknown> = Promise.resolve()

  // Settles once the file is saved, or on a failure, the channel closing, or the user stopping it.
  const saved = new Promise<string>((resolve, reject) => {
    const then = (step: () => Promise<unknown>) => {
      saving = saving.then(step)
      saving.catch(reject)
    }
    const nonsense = () => reject(new Error('The answer from the other PC didn’t make sense.'))
    let size: number | null = null
    let parts: ArrayBuffer[] = []
    let gathered = 0
    let finished = false
    const write = () => {
      const data = join(parts, gathered)
      parts = []
      gathered = 0
      then(() => file.sink!.write(data))
    }
    channel.onmessage = (e) => {
      if (typeof e.data === 'string') {
        let msg: CopiedFile | FileReply
        try {
          msg = JSON.parse(e.data)
        } catch {
          return nonsense()
        }
        if ('t' in msg) return reject(new HostError(msg.t === 'failed' ? msg.message : 'it stopped sending the file'))
        if (size != null) return nonsense()
        size = msg.size
        const name = msg.name
        then(async () => void (file.sink = await open(name)))
      } else {
        if (size == null) return nonsense()
        const data = e.data as ArrayBuffer
        parts.push(data)
        gathered += data.byteLength
        received += data.byteLength
        if (gathered >= WRITE_SIZE) write()
      }
      if (!finished && received >= size) {
        finished = true
        if (gathered) write()
        then(async () => resolve(await file.sink!.finish()))
      }
    }
    channel.onclose = () => reject(new Error('The connection closed before the file arrived.'))
    signal.addEventListener('abort', () => reject(signal.reason), { once: true })
  })

  try {
    if (channel.readyState !== 'open') {
      await Promise.race([new Promise((r) => channel.addEventListener('open', r, { once: true })), saved])
    }
    channel.send(JSON.stringify(request))
    return await saved
  } catch (e) {
    // Whatever was written is thrown away, once the write under way is done.
    channel.onmessage = null
    await saving.catch(() => {})
    await file.sink?.abort().catch(() => {})
    throw e
  } finally {
    clearInterval(timer)
    progress(received)
    channel.onmessage = null
    channel.onclose = null
    channel.close()
  }
}

function join(parts: ArrayBuffer[], size: number): ArrayBuffer {
  if (parts.length === 1) return parts[0]
  const out = new Uint8Array(size)
  let at = 0
  for (const part of parts) {
    out.set(new Uint8Array(part), at)
    at += part.byteLength
  }
  return out.buffer
}
