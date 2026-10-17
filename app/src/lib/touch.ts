import type { ClientMsg, MouseButton } from './types'

type Send = (msg: ClientMsg) => void

// Fingers drive the remote pointer like a laptop trackpad, the way Microsoft's
// Remote Desktop app does on phones:
//   drag: move the pointer        tap: click         double-tap and drag: drag
//   touch and hold: right-click   two-finger tap: right-click
//   two-finger drag: scroll       pinch: zoom (the view follows the pointer)
// Pens and mice point directly and are handled by attachDesktopInput.

const TAP_MS = 250
const TAP_SLOP = 10
const DOUBLE_TAP_MS = 300
const DOUBLE_TAP_SLOP = 40
const LONG_PRESS_MS = 500
const PINCH_SLOP = 24
const SCROLL_SLOP = 10
const MAX_ZOOM = 6
/** How close the pointer may get to the edge of a zoomed view, as a share of the view. */
const EDGE = 0.12
/** Windows wheel units per remote pixel, matching ~100px per 120-unit notch. */
const WHEEL_PER_PIXEL = 1.2

type Mode = 'idle' | 'press' | 'move' | 'drag' | 'two' | 'pinch' | 'scroll' | 'done'

interface Point {
  x: number
  y: number
}

export interface TouchInputOptions {
  /** Width of the remote display in its own pixels, so scrolling follows the finger. */
  displayWidth(): number
}

export function attachTouchInput(
  surface: HTMLElement,
  video: HTMLVideoElement,
  cursor: HTMLElement,
  send: Send,
  opts: TouchInputOptions,
): () => void {
  // Pointer position on the remote display (0..1), and whether it's drawn here.
  let cx = 0.5
  let cy = 0.5
  let showCursor = false
  // View transform: scale z around the surface's top left, then translate.
  let z = 1
  let tx = 0
  let ty = 0

  const fingers = new Map<number, Point>()
  let mode: Mode = 'idle'
  let start = { x: 0, y: 0, at: 0 }
  let lastTap = { x: 0, y: 0, at: -Infinity }
  let dragArmed = false
  let longPress: number | undefined
  let two = { d0: 0, m0: { x: 0, y: 0 }, z0: 1, tx0: 0, ty0: 0, at: 0, last: { x: 0, y: 0 }, lastAt: 0 }
  let speed = 0
  let lastMoveAt = 0

  let moveFrame = 0
  let wheelX = 0
  let wheelY = 0
  let wheelFrame = 0
  // Recent finger positions while scrolling, to measure how fast they were going on release.
  let trail: { at: number; x: number; y: number }[] = []
  let velocity = { x: 0, y: 0 }
  let glideFrame = 0

  /** Where the video image sits in the surface before zooming. */
  function base() {
    const W = surface.clientWidth
    const H = surface.clientHeight
    const vw = video.videoWidth || W
    const vh = video.videoHeight || H
    const s = Math.min(W / vw, H / vh)
    const w = vw * s
    const h = vh * s
    return { W, H, x: (W - w) / 2, y: (H - h) / 2, w, h }
  }

  // Keep the image filling the view on an axis where it's bigger than the view, centered otherwise.
  function clampAxis(t: number, start: number, size: number, view: number) {
    const scaled = size * z
    if (scaled <= view) return (view - scaled) / 2 - start * z
    return Math.min(-start * z, Math.max(view - (start + size) * z, t))
  }

  function cursorAt(b: ReturnType<typeof base>): Point {
    return { x: (b.x + cx * b.w) * z + tx, y: (b.y + cy * b.h) * z + ty }
  }

  function apply(b = base()) {
    tx = clampAxis(tx, b.x, b.w, b.W)
    ty = clampAxis(ty, b.y, b.h, b.H)
    video.style.transformOrigin = '0 0'
    video.style.transform = z === 1 ? '' : `translate(${tx}px, ${ty}px) scale(${z})`
    cursor.hidden = !showCursor
    if (showCursor) {
      const p = cursorAt(b)
      cursor.style.transform = `translate(${p.x}px, ${p.y}px)`
    }
  }

  /** Pan a zoomed view so the pointer stays away from its edges. */
  function follow(b: ReturnType<typeof base>) {
    if (z === 1) return
    const p = cursorAt(b)
    const mx = b.W * EDGE
    const my = b.H * EDGE
    if (p.x < mx) tx += mx - p.x
    else if (p.x > b.W - mx) tx -= p.x - (b.W - mx)
    if (p.y < my) ty += my - p.y
    else if (p.y > b.H - my) ty -= p.y - (b.H - my)
  }

  function sendMove() {
    moveFrame = 0
    send({ t: 'mouseMove', x: cx, y: cy })
  }

  function flushMove() {
    if (moveFrame) cancelAnimationFrame(moveFrame)
    sendMove()
  }

  function moveBy(dx: number, dy: number, now: number) {
    const dt = Math.max(1, now - lastMoveAt)
    lastMoveAt = now
    // Slow movements are precise; quick flicks cover the screen.
    speed = speed * 0.6 + (Math.hypot(dx, dy) / dt) * 0.4
    const gain = Math.min(3, 1 + speed * 1.5)
    const b = base()
    cx = Math.min(1, Math.max(0, cx + (dx * gain) / (b.w * z)))
    cy = Math.min(1, Math.max(0, cy + (dy * gain) / (b.h * z)))
    showCursor = true
    follow(b)
    apply(b)
    if (!moveFrame) moveFrame = requestAnimationFrame(sendMove)
  }

  function click(button: MouseButton) {
    flushMove()
    send({ t: 'mouseButton', button, down: true })
    send({ t: 'mouseButton', button, down: false })
  }

  function sendWheel() {
    wheelFrame = 0
    const dx = Math.trunc(wheelX)
    const dy = Math.trunc(wheelY)
    if (!dx && !dy) return
    wheelX -= dx
    wheelY -= dy
    send({ t: 'wheel', dx, dy })
  }

  /** Scroll so the remote content moves with a finger that moved (dx, dy) on screen. */
  function scrollBy(dx: number, dy: number) {
    const remotePerScreen = opts.displayWidth() / (base().w * z)
    wheelX -= dx * remotePerScreen * WHEEL_PER_PIXEL
    wheelY -= dy * remotePerScreen * WHEEL_PER_PIXEL
    if (!wheelFrame) wheelFrame = requestAnimationFrame(sendWheel)
  }

  function glide() {
    let last = performance.now()
    const step = (now: number) => {
      const dt = now - last
      last = now
      const decay = Math.exp(-dt / 325)
      velocity.x *= decay
      velocity.y *= decay
      scrollBy(velocity.x * dt, velocity.y * dt)
      glideFrame = Math.hypot(velocity.x, velocity.y) > 0.02 ? requestAnimationFrame(step) : 0
    }
    glideFrame = requestAnimationFrame(step)
  }

  function stopGlide() {
    cancelAnimationFrame(glideFrame)
    glideFrame = 0
  }

  function local(e: PointerEvent): Point {
    const r = surface.getBoundingClientRect()
    return { x: e.clientX - r.left, y: e.clientY - r.top }
  }

  function pair(): [Point, Point] {
    const [a, b] = fingers.values()
    return [a, b]
  }

  function clearLongPress() {
    clearTimeout(longPress)
    longPress = undefined
  }

  function endDrag() {
    if (mode === 'drag') {
      flushMove()
      send({ t: 'mouseButton', button: 'left', down: false })
    }
  }

  const onDown = (e: PointerEvent) => {
    if (e.pointerType !== 'touch') {
      // A mouse or pen points directly; the drawn pointer would only get in the way.
      showCursor = false
      apply()
      return
    }
    // Keeps focus (and an open on-screen keyboard) where it is.
    e.preventDefault()
    stopGlide()
    const p = local(e)
    fingers.set(e.pointerId, p)
    const now = performance.now()

    if (fingers.size === 1) {
      mode = 'press'
      start = { ...p, at: now }
      lastMoveAt = now
      speed = 0
      dragArmed = now - lastTap.at < DOUBLE_TAP_MS && Math.hypot(p.x - lastTap.x, p.y - lastTap.y) < DOUBLE_TAP_SLOP
      if (!dragArmed) {
        longPress = window.setTimeout(() => {
          if (mode !== 'press' || !showCursor) return
          mode = 'done'
          click('right')
          navigator.vibrate?.(10)
        }, LONG_PRESS_MS)
      }
    } else if (fingers.size === 2) {
      clearLongPress()
      endDrag()
      const [a, b] = pair()
      const m = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }
      mode = 'two'
      two = { d0: Math.hypot(a.x - b.x, a.y - b.y), m0: m, z0: z, tx0: tx, ty0: ty, at: now, last: m, lastAt: now }
      trail = []
    } else {
      clearLongPress()
      endDrag()
      mode = 'done'
    }
  }

  const onMove = (e: PointerEvent) => {
    if (e.pointerType !== 'touch') {
      const b = base()
      const r = video.getBoundingClientRect()
      cx = Math.min(1, Math.max(0, (e.clientX - r.left - b.x * z) / (b.w * z)))
      cy = Math.min(1, Math.max(0, (e.clientY - r.top - b.y * z) / (b.h * z)))
      if (showCursor) {
        showCursor = false
        apply(b)
      }
      return
    }
    const prev = fingers.get(e.pointerId)
    if (!prev) return
    const p = local(e)
    const now = performance.now()

    switch (mode) {
      case 'press':
        if (Math.hypot(p.x - start.x, p.y - start.y) < TAP_SLOP) return
        clearLongPress()
        if (dragArmed) {
          flushMove()
          send({ t: 'mouseButton', button: 'left', down: true })
          mode = 'drag'
        } else {
          mode = 'move'
        }
        moveBy(p.x - prev.x, p.y - prev.y, now)
        break
      case 'move':
      case 'drag':
        moveBy(p.x - prev.x, p.y - prev.y, now)
        break
      case 'two':
      case 'pinch':
      case 'scroll': {
        fingers.set(e.pointerId, p)
        const [a, b] = pair()
        const m = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 }
        const d = Math.hypot(a.x - b.x, a.y - b.y)
        if (mode === 'two') {
          if (Math.abs(d - two.d0) > PINCH_SLOP) mode = 'pinch'
          else if (Math.hypot(m.x - two.m0.x, m.y - two.m0.y) > SCROLL_SLOP) mode = 'scroll'
          else return
        }
        if (mode === 'pinch') {
          // Zoom around the fingers: the spot that was under them stays under them.
          z = Math.min(MAX_ZOOM, Math.max(1, (two.z0 * d) / Math.max(1, two.d0)))
          tx = m.x - ((two.m0.x - two.tx0) / two.z0) * z
          ty = m.y - ((two.m0.y - two.ty0) / two.z0) * z
          apply()
        } else {
          scrollBy(m.x - two.last.x, m.y - two.last.y)
          // Both fingers report separately, often in the same millisecond, so measure over a window.
          trail.push({ at: now, ...m })
          while (trail.length > 2 && now - trail[0].at > 100) trail.shift()
        }
        two.last = m
        two.lastAt = now
        return
      }
      default:
        break
    }
    fingers.set(e.pointerId, p)
  }

  const onUp = (e: PointerEvent) => {
    if (e.pointerType !== 'touch' || !fingers.has(e.pointerId)) return
    const now = performance.now()
    const cancelled = e.type === 'pointercancel'
    clearLongPress()

    if (mode === 'press' && fingers.size === 1 && !cancelled && now - start.at < TAP_MS) {
      if (showCursor) {
        click('left')
      } else {
        // The first tap shows where the pointer is instead of clicking somewhere unseen.
        showCursor = true
        apply()
        flushMove()
      }
      lastTap = { x: start.x, y: start.y, at: now }
    } else if (mode === 'drag') {
      endDrag()
    } else if (mode === 'two' && !cancelled && now - two.at < TAP_MS * 1.5) {
      if (!showCursor) {
        showCursor = true
        apply()
      }
      click('right')
    } else if (mode === 'scroll' && !cancelled && now - two.lastAt < 50 && trail.length > 1) {
      const first = trail[0]
      const span = Math.max(16, now - first.at)
      velocity = { x: (two.last.x - first.x) / span, y: (two.last.y - first.y) / span }
      glide()
    }

    fingers.delete(e.pointerId)
    // Lifting one finger of two ends the gesture; the other one does nothing until it lifts too.
    mode = fingers.size === 0 ? 'idle' : 'done'
  }

  const onResize = () => apply()
  const observer = new ResizeObserver(onResize)
  observer.observe(surface)

  surface.addEventListener('pointerdown', onDown)
  surface.addEventListener('pointermove', onMove)
  surface.addEventListener('pointerup', onUp)
  surface.addEventListener('pointercancel', onUp)
  video.addEventListener('resize', onResize)

  return () => {
    clearLongPress()
    stopGlide()
    cancelAnimationFrame(moveFrame)
    cancelAnimationFrame(wheelFrame)
    endDrag()
    observer.disconnect()
    surface.removeEventListener('pointerdown', onDown)
    surface.removeEventListener('pointermove', onMove)
    surface.removeEventListener('pointerup', onUp)
    surface.removeEventListener('pointercancel', onUp)
    video.removeEventListener('resize', onResize)
    video.style.transform = ''
    cursor.hidden = true
  }
}
