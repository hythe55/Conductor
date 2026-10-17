import type { Sendable } from './files'

/** What the Android app gives the page for files. See AndroidFiles.kt. */
interface AndroidFiles {
  shared(): string
  taken(): void
  clearShared(): void
  read(id: number, offset: number, length: number): string | null
  close(id: number): void
  savesToDownloads(): boolean
  beginSave(name: string): number
  writeSave(id: number, data: string): boolean
  finishSave(id: number): string
  abortSave(id: number): void
}

/** Only in the Android app. */
export const android = (window as unknown as { ConductorAndroid?: AndroidFiles }).ConductorAndroid

/** A file shared to the Android app from another app, read through it. */
export class SharedFile implements Sendable {
  readonly shared = true

  constructor(
    readonly id: number,
    readonly name: string,
    readonly size: number,
  ) {}

  slice(start: number, end: number) {
    return {
      arrayBuffer: async () => {
        const data = android?.read(this.id, start, Math.min(end, this.size) - start)
        if (data == null) throw new Error(`Couldn’t read ${this.name} on this phone.`)
        const bytes = fromBase64(data)
        // Smaller than the app said, which would otherwise never finish.
        if (!bytes.byteLength && end > start) throw new Error(`${this.name} ended early.`)
        return bytes
      },
    }
  }

  close() {
    android?.close(this.id)
  }
}

export function toBase64(data: ArrayBuffer): string {
  const bytes = new Uint8Array(data)
  let text = ''
  // In pieces, since a call takes only so many arguments.
  for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  return btoa(text)
}

function fromBase64(text: string): ArrayBuffer {
  const raw = atob(text)
  const bytes = new Uint8Array(raw.length)
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i)
  return bytes.buffer
}
