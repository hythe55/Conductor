import type { Device, WakeAdapter } from './types'

function ipv4(address: string): number {
  return address.split('.').reduce((n, part) => n * 256 + Number(part), 0)
}

function sameNetwork(a: WakeAdapter, b: WakeAdapter): boolean {
  if (a.prefix !== b.prefix) return false
  const mask = a.prefix === 0 ? 0 : (~0 << (32 - a.prefix)) >>> 0
  return (ipv4(a.address) & mask) >>> 0 === (ipv4(b.address) & mask) >>> 0
}

/**
 * A PC that's on and on the same network as `target`, which can send the
 * packets that wake it. The device this runs on comes first, then the PC
 * serving this page.
 */
export function wakerFor(target: Device, all: Device[]): Device | null {
  const theirs = target.wake?.adapters ?? []
  if (theirs.length === 0) return null
  const rank = (d: Device) => (d.isYou ? 0 : d.isSelf ? 1 : 2)
  const candidates = all.filter((d) => d.id !== target.id && d.host?.wake).sort((a, b) => rank(a) - rank(b))
  return candidates.find((d) => d.host!.wake!.adapters.some((a) => theirs.some((t) => sameNetwork(a, t)))) ?? null
}

/** Checks `test` every `every` ms until it's true or `within` ms pass. */
export async function waitUntil(test: () => Promise<boolean>, within: number, every = 2000): Promise<boolean> {
  const deadline = Date.now() + within
  for (;;) {
    if (await test()) return true
    if (Date.now() + every > deadline) return false
    await new Promise((r) => setTimeout(r, every))
  }
}
