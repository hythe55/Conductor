import type { ClientMsg, MouseButton } from './types'

type Send = (msg: ClientMsg) => void

const BUTTONS: MouseButton[] = ['left', 'middle', 'right', 'back', 'forward']

/** Where the video image actually sits inside its element (object-fit: contain). */
export function contentRect(video: HTMLVideoElement): DOMRect {
  const box = video.getBoundingClientRect()
  const vw = video.videoWidth || box.width
  const vh = video.videoHeight || box.height
  const scale = Math.min(box.width / vw, box.height / vh)
  const w = vw * scale
  const h = vh * scale
  return new DOMRect(box.left + (box.width - w) / 2, box.top + (box.height - h) / 2, w, h)
}

function normalized(video: HTMLVideoElement, clientX: number, clientY: number) {
  const r = contentRect(video)
  return {
    x: Math.min(1, Math.max(0, (clientX - r.left) / r.width)),
    y: Math.min(1, Math.max(0, (clientY - r.top) / r.height)),
  }
}

export interface DesktopInputOptions {
  /** Return true to keep a key combo on this device instead of sending it. */
  localShortcut(e: KeyboardEvent): boolean
}

/** Mouse, wheel and keyboard for desktop browsers and the desktop app. */
export function attachDesktopInput(
  surface: HTMLElement,
  video: HTMLVideoElement,
  send: Send,
  opts: DesktopInputOptions,
): () => void {
  let pending: { x: number; y: number } | null = null
  let frame = 0
  let wheelX = 0
  let wheelY = 0

  const flushMove = () => {
    frame = 0
    if (pending) send({ t: 'mouseMove', ...pending })
    pending = null
  }

  const onMove = (e: PointerEvent) => {
    if (e.pointerType === 'touch') return
    pending = normalized(video, e.clientX, e.clientY)
    if (!frame) frame = requestAnimationFrame(flushMove)
  }

  const onDown = (e: PointerEvent) => {
    if (e.pointerType === 'touch') return
    e.preventDefault()
    surface.focus()
    surface.setPointerCapture(e.pointerId)
    send({ t: 'mouseMove', ...normalized(video, e.clientX, e.clientY) })
    const button = BUTTONS[e.button]
    if (button) send({ t: 'mouseButton', button, down: true })
  }

  const onUp = (e: PointerEvent) => {
    if (e.pointerType === 'touch') return
    e.preventDefault()
    const button = BUTTONS[e.button]
    if (button) send({ t: 'mouseButton', button, down: false })
  }

  const onWheel = (e: WheelEvent) => {
    e.preventDefault()
    // Windows counts 120 per notch; browsers report ~100px per notch.
    const scale = e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 360 : 1.2
    wheelX += e.deltaX * scale
    wheelY += e.deltaY * scale
    const dx = Math.trunc(wheelX)
    const dy = Math.trunc(wheelY)
    if (dx || dy) {
      wheelX -= dx
      wheelY -= dy
      send({ t: 'wheel', dx, dy })
    }
  }

  const onKey = (e: KeyboardEvent) => {
    if (opts.localShortcut(e)) return
    e.preventDefault()
    e.stopPropagation()
    if (e.code && e.code !== 'Unidentified') {
      send({ t: 'key', code: e.code, down: e.type === 'keydown' })
    } else if (e.type === 'keydown' && e.key.length === 1) {
      send({ t: 'text', text: e.key })
    }
  }

  const release = () => send({ t: 'releaseAll' })
  const noMenu = (e: Event) => e.preventDefault()

  surface.addEventListener('pointermove', onMove)
  surface.addEventListener('pointerdown', onDown)
  surface.addEventListener('pointerup', onUp)
  surface.addEventListener('wheel', onWheel, { passive: false })
  surface.addEventListener('contextmenu', noMenu)
  window.addEventListener('keydown', onKey, true)
  window.addEventListener('keyup', onKey, true)
  window.addEventListener('blur', release)

  return () => {
    cancelAnimationFrame(frame)
    surface.removeEventListener('pointermove', onMove)
    surface.removeEventListener('pointerdown', onDown)
    surface.removeEventListener('pointerup', onUp)
    surface.removeEventListener('wheel', onWheel)
    surface.removeEventListener('contextmenu', noMenu)
    window.removeEventListener('keydown', onKey, true)
    window.removeEventListener('keyup', onKey, true)
    window.removeEventListener('blur', release)
    release()
  }
}
