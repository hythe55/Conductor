<script lang="ts">
  import { onDestroy, untrack } from 'svelte'
  import { Problem } from '../lib/errors'
  import { attachDesktopInput } from '../lib/input'
  import { RemoteSession, type SessionState, type Stats } from '../lib/session'
  import { readClipboard, writeClipboard } from '../lib/platform'
  import { attachTouchInput } from '../lib/touch'
  import type { DisplayInfo, HostInfo } from '../lib/types'
  import Handle from '../components/Handle.svelte'
  import Keyboard from '../components/Keyboard.svelte'
  import ProblemText from '../components/ProblemText.svelte'

  let { url, info, onclose }: { url: string; info: HostInfo; onclose: () => void } = $props()

  let video: HTMLVideoElement
  let surface: HTMLDivElement
  let pointer: HTMLDivElement
  let keyboard: Keyboard
  // The view is created per connection, so the initial props are all it needs.
  const start = untrack(() => ({
    url,
    displays: info.displays,
    display: info.displays.find((d) => d.primary)?.id ?? info.displays[0]?.id ?? '',
  }))

  let phase = $state<SessionState>('connecting')
  let error = $state<Problem | null>(null)
  let hasVideo = $state(false)
  let displays = $state<DisplayInfo[]>(start.displays)
  let activeDisplay = $state<string>(start.display)
  let panelOpen = $state(false)
  let showStats = $state(false)
  let stats = $state<Stats | null>(null)
  let fullscreen = $state(false)
  let keyboardOpen = $state(false)
  // The remote pointer's shape, shown in place of this device's pointer.
  let cursor = $state('default')
  // Height of the part of the window the phone keyboard leaves uncovered.
  let viewHeight = $state<number | null>(null)

  const coarse = window.matchMedia('(pointer: coarse)').matches

  // Clipboard text both devices have, and text copied there that couldn't be put on this device's clipboard yet.
  let sharedText: string | null = null
  let notCopiedHere: string | null = null

  function shareClipboard(text: string | null) {
    // The host doesn't take more than this either.
    if (text == null || text === sharedText || text.length > 120 * 1024) return
    sharedText = text
    session.send({ t: 'clipboard', text })
  }

  async function copyHere(text: string) {
    sharedText = text
    notCopiedHere = (await writeClipboard(text)) ? null : text
  }

  // Coming back to this window: whatever was copied meanwhile goes to the side that doesn't have it.
  async function syncClipboard() {
    if (notCopiedHere != null) await copyHere(notCopiedHere)
    else shareClipboard(await readClipboard())
  }

  const dpr = window.devicePixelRatio || 1
  const session = new RemoteSession(
    start.url,
    {
      display: start.display || undefined,
      maxWidth: Math.round(screen.width * dpr),
      maxHeight: Math.round(screen.height * dpr),
    },
    {
      onState(s, e) {
        phase = s
        if (e) error = e
      },
      onStream(stream) {
        video.srcObject = stream
        video.play().catch(() => {})
      },
      onMessage(msg) {
        if (msg.t === 'displays') {
          displays = msg.displays
          activeDisplay = msg.active
          session.options.display = msg.active
        } else if (msg.t === 'error') {
          error = new Problem(`Couldn’t show the screen of ${info.name}. Try another display, or reconnect.`, msg.message)
        } else if (msg.t === 'clipboard') {
          void copyHere(msg.text)
        } else if (msg.t === 'cursor') {
          cursor = msg.name
        }
      },
      onOpen() {
        void syncClipboard()
      },
    },
  )
  session.connect()

  function chord(e: KeyboardEvent, key: string) {
    return e.ctrlKey && e.altKey && e.shiftKey && e.code === key
  }

  function localShortcut(e: KeyboardEvent): boolean {
    // Keys reach the remote PC only while its screen has focus; menus and buttons keep theirs.
    if (e.target !== surface) {
      if (panelOpen && e.type === 'keydown' && chord(e, 'KeyM')) {
        e.preventDefault()
        panelOpen = false
      }
      return true
    }
    if (e.type !== 'keydown') return chord(e, e.code) && ['KeyM', 'KeyS', 'KeyQ', 'KeyF'].includes(e.code)
    if (chord(e, 'KeyM')) panelOpen = !panelOpen
    else if (chord(e, 'KeyS')) showStats = !showStats
    else if (chord(e, 'KeyQ')) disconnect()
    else if (chord(e, 'KeyF')) toggleFullscreen()
    else return false
    e.preventDefault()
    session.send({ t: 'releaseAll' })
    return true
  }

  // Typing goes back to the remote PC as soon as the menu closes.
  $effect(() => {
    if (!panelOpen && !keyboardOpen) surface?.focus()
  })

  $effect(() => {
    const send = (m: Parameters<typeof session.send>[0]) => session.send(m)
    const detachDesktop = attachDesktopInput(surface, video, send, { localShortcut, onPaste: shareClipboard })
    const detachTouch = attachTouchInput(surface, video, pointer, send, {
      displayWidth: () => displays.find((d) => d.id === activeDisplay)?.width || video.videoWidth || 1920,
    })
    surface.focus()
    return () => {
      detachDesktop()
      detachTouch()
    }
  })

  $effect(() => {
    const vv = window.visualViewport
    if (!vv) return
    const update = () => (viewHeight = window.innerHeight - vv.height > 1 ? vv.height : null)
    update()
    vv.addEventListener('resize', update)
    return () => vv.removeEventListener('resize', update)
  })

  $effect(() => {
    const onVisibility = () => session.send({ t: 'setPaused', paused: document.hidden })
    const onFullscreen = () => (fullscreen = !!document.fullscreenElement)
    const onFocus = () => void syncClipboard()
    document.addEventListener('visibilitychange', onVisibility)
    document.addEventListener('fullscreenchange', onFullscreen)
    window.addEventListener('focus', onFocus)
    return () => {
      document.removeEventListener('visibilitychange', onVisibility)
      document.removeEventListener('fullscreenchange', onFullscreen)
      window.removeEventListener('focus', onFocus)
    }
  })

  $effect(() => {
    if (!showStats) {
      stats = null
      return
    }
    const timer = setInterval(async () => (stats = await session.stats()), 1000)
    return () => clearInterval(timer)
  })

  function selectDisplay(id: string) {
    if (id === activeDisplay) return
    activeDisplay = id
    session.options.display = id
    session.send({ t: 'selectDisplay', id })
    panelOpen = false
  }

  async function toggleFullscreen() {
    try {
      if (document.fullscreenElement) await document.exitFullscreen()
      else {
        await document.documentElement.requestFullscreen()
        // Lets Esc, Alt+Tab and the Windows key reach the remote PC where supported.
        const kb = (navigator as Navigator & { keyboard?: { lock?: () => Promise<void> } }).keyboard
        await kb?.lock?.().catch(() => {})
      }
    } catch {
      // Fullscreen refused (e.g. not from a user gesture); nothing to do.
    }
  }

  function disconnect() {
    session.close()
    if (document.fullscreenElement) document.exitFullscreen().catch(() => {})
    onclose()
  }

  function retry() {
    error = null
    void session.connect()
  }

  onDestroy(() => session.close())

  const quality = $derived(
    phase === 'reconnecting' ? 'bad' : stats && stats.rttMs != null && stats.rttMs > 80 ? 'poor' : 'good',
  )
</script>

<div class="stream" style:height={viewHeight ? `${viewHeight}px` : null}>
  <!-- Focus stays here for the whole session, so it has no outline: one would frame the remote screen. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="surface"
    bind:this={surface}
    style:cursor
    tabindex="0"
    role="application"
    aria-label="Screen of {info.name}"
  >
    <!-- svelte-ignore a11y_media_has_caption -->
    <video
      bind:this={video}
      class:ready={hasVideo}
      autoplay
      playsinline
      muted
      disablepictureinpicture
      onloadeddata={() => (hasVideo = true)}
    ></video>
  </div>

  <div class="pointer" bind:this={pointer} hidden aria-hidden="true">
    <svg width="20" height="20" viewBox="0 0 20 20">
      <path
        d="M1.5 1.5v14.5l3.8-3.7 2.6 6.1 2.6-1.1-2.6-6h5.3z"
        fill="#fff"
        stroke="#000"
        stroke-width="1.2"
        stroke-linejoin="round"
      />
    </svg>
  </div>

  <Keyboard bind:this={keyboard} bind:open={keyboardOpen} send={(m) => session.send(m)} />

  {#if phase === 'failed'}
    <div class="notice" role="alert">
      <ProblemText problem={error ?? new Problem(`Lost the connection to ${info.name}.`)} />
      <div class="actions">
        <button class="btn primary" onclick={retry}>Try again</button>
        <button class="btn" onclick={onclose}>Back to devices</button>
      </div>
    </div>
  {:else if error && !hasVideo}
    <div class="notice" role="alert">
      <ProblemText problem={error} />
    </div>
  {:else if !hasVideo}
    <div class="notice quiet" aria-live="polite">
      <p>Connecting to {info.name}…</p>
    </div>
  {/if}

  {#if showStats && stats}
    <div class="stats" aria-label="Connection details">
      <span>{stats.width}×{stats.height} at {stats.fps} fps</span>
      <span>{(stats.bitrateKbps / 1000).toFixed(1)} Mbps</span>
      {#if stats.rttMs != null}<span>{stats.rttMs} ms round trip</span>{/if}
      {#if stats.decoder}<span>Decoder: {stats.decoder}</span>{/if}
      {#if stats.dropped || stats.lost}<span>{stats.dropped} frames dropped, {stats.lost} packets lost</span>{/if}
    </div>
  {/if}

  {#if phase !== 'failed'}
    <Handle
      bind:open={panelOpen}
      {quality}
      reconnecting={phase === 'reconnecting'}
      {displays}
      {activeDisplay}
      {fullscreen}
      {showStats}
      onselectdisplay={selectDisplay}
      ontogglefullscreen={toggleFullscreen}
      ontogglestats={() => (showStats = !showStats)}
      onkeyboard={coarse ? () => keyboard.show() : undefined}
      ondisconnect={disconnect}
    />
  {/if}
</div>

<style>
  .stream {
    position: fixed;
    inset: 0;
    background: var(--stream-bg);
    color: var(--overlay-text);
    /* The session view is always dark, so it uses the dark-theme accent. */
    --accent: var(--overlay-accent);
    --accent-hover: var(--overlay-accent-hover);
    --accent-text: var(--overlay-accent-text);
    --focus: var(--overlay-accent);
    --details-color: var(--overlay-muted);
    overflow: hidden;
  }

  .surface {
    position: absolute;
    inset: 0;
    outline: none;
    touch-action: none;
    user-select: none;
    -webkit-user-select: none;
    -webkit-touch-callout: none;
    -webkit-tap-highlight-color: transparent;
  }

  .pointer {
    position: absolute;
    top: 0;
    left: 0;
    margin: -1.5px 0 0 -1.5px;
    pointer-events: none;
  }

  video {
    width: 100%;
    height: 100%;
    object-fit: contain;
    opacity: 0;
    pointer-events: none;
  }

  video.ready {
    opacity: 1;
  }

  .notice {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-4);
    padding: var(--space-6);
    text-align: center;
  }

  .notice :global(p) {
    margin: 0;
    max-width: 48ch;
  }

  .notice.quiet {
    pointer-events: none;
    color: var(--overlay-muted);
  }

  .actions {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
    justify-content: center;
  }

  .actions .btn:not(.primary) {
    background: transparent;
    border-color: var(--overlay-border-strong);
    color: var(--overlay-text);
  }

  .actions .btn:not(.primary):hover {
    background: var(--overlay-hover);
  }

  .stats {
    position: absolute;
    top: max(var(--space-3), env(safe-area-inset-top));
    left: max(var(--space-3), env(safe-area-inset-left));
    display: flex;
    flex-direction: column;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-sm);
    background: var(--overlay-bg);
    font-size: var(--text-sm);
    font-variant-numeric: tabular-nums;
    pointer-events: none;
  }
</style>
