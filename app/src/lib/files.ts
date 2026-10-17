import type { FileReply } from './types'

// Each file goes on a data channel of its own: its name and size, then its
// bytes. The host answers once it's saved, and closing the channel early
// makes it delete what arrived.

export const FILE_CHANNEL = 'file'

const CHUNK = 64 * 1024
// Reading ahead of the network only fills memory, so wait while this much is queued.
const QUEUED_MAX = 1024 * 1024

/** The host couldn't save the file. */
export class SaveError extends Error {}

/**
 * Sends one file. Resolves with the name the host saved it under. `progress`
 * is called with the bytes that have left this device so far.
 */
export async function sendFile(
  channel: RTCDataChannel,
  file: File,
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
        else reject(new SaveError(reply.message))
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
