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
  /** Gets this device's clipboard text when the user pastes, before the paste keys are sent. */
  onPaste?(text: string): void
  /** The host takes pens, with their pressure and tilt. Otherwise a pen works as a mouse. */
  pen?: boolean
}

// PointerEvent.buttons for a pen: touching, the button on its side, and its eraser end.
const PEN_TIP = 1
const PEN_BARREL = 2
const PEN_ERASER = 32

/** How long to wait for the browser's paste event before sending the paste keys anyway. */
const PASTE_WAIT_MS = 150

function isPaste(e: KeyboardEvent) {
  if (e.altKey || e.metaKey) return false
  return (e.ctrlKey && e.code === 'KeyV') || (e.shiftKey && !e.ctrlKey && e.code === 'Insert')
}

/** Mouse, pen, wheel and keyboard for desktop browsers and the desktop app. */
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
  // A paste key held back until the clipboard has been sent.
  let pendingPaste: string | null = null
  let pasteTimer: number | undefined

  const flushPaste = () => {
    clearTimeout(pasteTimer)
    if (pendingPaste) send({ t: 'key', code: pendingPaste, down: true })
    pendingPaste = null
  }

  // The browser reads the clipboard for a paste only if the key press goes through,
  // so let it through, take the text from the paste event, then send the keys.
  const onPasteEvent = (e: ClipboardEvent) => {
    if (!pendingPaste) return
    e.preventDefault()
    const text = e.clipboardData?.getData('text/plain')
    if (text) opts.onPaste?.(text)
    flushPaste()
  }

  const flushMove = () => {
    frame = 0
    if (pending) send({ t: 'mouseMove', ...pending })
    pending = null
  }

  const isPen = (e: PointerEvent) => !!opts.pen && e.pointerType === 'pen'
  // Browsers only say it's the eraser end while it touches, so hovering keeps
  // whichever end last touched until the pen moves away.
  let eraser = false

  const sendPen = (e: PointerEvent) => {
    if (e.buttons & (PEN_TIP | PEN_ERASER)) eraser = (e.buttons & PEN_ERASER) !== 0
    else if (e.button === 5) eraser = true
    send({
      t: 'pen',
      ...normalized(video, e.clientX, e.clientY),
      contact: (e.buttons & (PEN_TIP | PEN_ERASER)) !== 0,
      pressure: e.pressure,
      tiltX: Math.round(e.tiltX),
      tiltY: Math.round(e.tiltY),
      twist: Math.round(e.twist),
      barrel: (e.buttons & PEN_BARREL) !== 0,
      eraser,
    })
  }

  const onMove = (e: PointerEvent) => {
    if (e.pointerType === 'touch') return
    if (isPen(e)) {
      // Every point the pen passed through since the last event, for smooth strokes.
      const points = e.getCoalescedEvents?.() ?? []
      for (const p of points.length ? points : [e]) sendPen(p)
      return
    }
    pending = normalized(video, e.clientX, e.clientY)
    if (!frame) frame = requestAnimationFrame(flushMove)
  }

  const onDown = (e: PointerEvent) => {
    if (e.pointerType === 'touch') return
    e.preventDefault()
    surface.focus()
    surface.setPointerCapture(e.pointerId)
    if (isPen(e)) return sendPen(e)
    send({ t: 'mouseMove', ...normalized(video, e.clientX, e.clientY) })
    const button = BUTTONS[e.button]
    if (button) send({ t: 'mouseButton', button, down: true })
  }

  const onUp = (e: PointerEvent) => {
    if (e.pointerType === 'touch') return
    e.preventDefault()
    if (isPen(e)) return sendPen(e)
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
    e.stopPropagation()
    if (opts.onPaste && e.type === 'keydown' && isPaste(e)) {
      flushPaste()
      pendingPaste = e.code
      pasteTimer = window.setTimeout(flushPaste, PASTE_WAIT_MS)
      return
    }
    e.preventDefault()
    if (pendingPaste === e.code) flushPaste()
    if (e.code && e.code !== 'Unidentified') {
      send({ t: 'key', code: e.code, down: e.type === 'keydown' })
    } else if (e.type === 'keydown' && e.key.length === 1) {
      send({ t: 'text', text: e.key })
    }
  }

  // Out of range, or the browser took it over.
  const onPenGone = (e: PointerEvent) => {
    if (!isPen(e)) return
    eraser = false
    send({ t: 'penLeave' })
  }

  const release = () => send({ t: 'releaseAll' })
  const noMenu = (e: Event) => e.preventDefault()

  surface.addEventListener('pointermove', onMove)
  surface.addEventListener('pointerdown', onDown)
  surface.addEventListener('pointerup', onUp)
  surface.addEventListener('pointerleave', onPenGone)
  surface.addEventListener('pointercancel', onPenGone)
  surface.addEventListener('wheel', onWheel, { passive: false })
  surface.addEventListener('contextmenu', noMenu)
  window.addEventListener('keydown', onKey, true)
  window.addEventListener('keyup', onKey, true)
  window.addEventListener('blur', release)
  surface.addEventListener('paste', onPasteEvent)

  return () => {
    cancelAnimationFrame(frame)
    clearTimeout(pasteTimer)
    surface.removeEventListener('paste', onPasteEvent)
    surface.removeEventListener('pointermove', onMove)
    surface.removeEventListener('pointerdown', onDown)
    surface.removeEventListener('pointerup', onUp)
    surface.removeEventListener('pointerleave', onPenGone)
    surface.removeEventListener('pointercancel', onPenGone)
    surface.removeEventListener('wheel', onWheel)
    surface.removeEventListener('contextmenu', noMenu)
    window.removeEventListener('keydown', onKey, true)
    window.removeEventListener('keyup', onKey, true)
    window.removeEventListener('blur', release)
    release()
  }
}
