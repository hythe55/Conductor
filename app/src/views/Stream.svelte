<script lang="ts">
  import { onDestroy, untrack } from 'svelte'
  import { Problem } from '../lib/errors'
  import { attachDesktopInput } from '../lib/input'
  import { RemoteSession, type SessionState, type Stats } from '../lib/session'
  import { readClipboard, writeClipboard } from '../lib/platform'
  import { share } from '../lib/share.svelte'
  import { attachTouchInput } from '../lib/touch'
  import { Transfers } from '../lib/transfers.svelte'
  import type { DisplayInfo, HostInfo, Route, ShownWindow, WindowInfo } from '../lib/types'
  import Handle from '../components/Handle.svelte'
  import Keyboard from '../components/Keyboard.svelte'
  import ProblemText from '../components/ProblemText.svelte'

  let { url, info, onclose }: { url: string; info: HostInfo; onclose: () => void } = $props()

  let video: HTMLVideoElement
  let surface: HTMLDivElement
  let pointer: HTMLDivElement
  let keyboard: Keyboard
  let picker: HTMLInputElement
  // The view is created per connection, so the initial props are all it needs.
  const start = untrack(() => ({
    url,
    name: info.name,
    version: info.version,
    displays: info.displays,
    display: info.displays.find((d) => d.primary)?.id ?? info.displays[0]?.id ?? '',
  }))

  let phase = $state<SessionState>('connecting')
  let error = $state<Problem | null>(null)
  let hasVideo = $state(false)
  let displays = $state<DisplayInfo[]>(start.displays)
  let activeDisplay = $state<string>(start.display)
  // Hosts before 0.1.10 can only show whole displays.
  const canShowWindows = atLeast(start.version, [0, 1, 10])
  let shownWindow = $state<ShownWindow | null>(null)
  // The windows open on the remote PC, while the menu lists them. Null until they arrive.
  let windows = $state<WindowInfo[] | null>(null)
  // The window asked for again after reconnecting, until the host answers.
  let restoring: string | null = null
  // Said on the handle for a moment when the shown window goes away.
  let windowGoneNote = $state(false)
  let windowGoneTimer: number | undefined
  let panelOpen = $state(false)
  let showStats = $state(false)
  let stats = $state<Stats | null>(null)
  let fullscreen = $state(false)
  let speaker: HTMLAudioElement
  // Sound from the remote PC plays unless turned off, which is remembered.
  const SOUND_KEY = 'conductor.sound'
  let sound = $state(savedSound())
  let soundLater: AbortController | undefined
  // Covering the remote PC's screen is remembered for each PC.
  const HIDE_KEY = `conductor.hideScreen.${start.name}`
  let hideScreen = $state(savedHideScreen())
  let route = $state<Route | null>(null)
  // Said on the handle once the connection has been relayed for a while. Tailscale
  // often starts relayed and finds a direct path within seconds.
  let relayedNote = $state(false)
  let relayTimer: number | undefined
  let keyboardOpen = $state(false)
  // The remote pointer's shape, shown in place of this device's pointer.
  let cursor = $state('default')
  // Height of the part of the window the phone keyboard leaves uncovered.
  let viewHeight = $state<number | null>(null)

  const coarse = window.matchMedia('(pointer: coarse)').matches

  // Files go to the Downloads folder on the remote PC, and files copied there can be saved here.
  const transfers = new Transfers(
    (label) => session.openChannel(label),
    () => info.name,
  )
  let dragging = $state(false)
  let dragDepth = 0

  // Files shared to the phone app go to this PC: those waiting when it connects, and any shared while connected.
  function sendShared() {
    if (share.files.length) transfers.send(share.take())
    else if (share.text || share.unreadable) {
      transfers.failure = {
        chip: share.text ? 'Only files can be sent' : 'Couldn’t read the shared file',
        problem: new Problem(
          share.text
            ? 'Only files can be sent to a PC, not text. To get text there, type it on the PC.'
            : 'Couldn’t read what was shared. Try sharing it again, or from another app.',
        ),
      }
      share.dismiss()
    }
  }

  // Files can be dropped while the remote screen is showing. The app stops
  // other drops from opening the file in place of Conductor.
  const canDrop = (e: DragEvent) => phase === 'connected' && (e.dataTransfer?.types.includes('Files') ?? false)

  function onDragEnter(e: DragEvent) {
    if (!canDrop(e)) return
    e.preventDefault()
    dragDepth++
    dragging = true
  }

  function onDragOver(e: DragEvent) {
    if (!canDrop(e)) return
    e.preventDefault()
    e.dataTransfer!.dropEffect = 'copy'
  }

  function onDragLeave() {
    if (dragDepth > 0 && --dragDepth === 0) dragging = false
  }

  function onDrop(e: DragEvent) {
    dragDepth = 0
    dragging = false
    if (!canDrop(e)) return
    e.preventDefault()
    const items = [...(e.dataTransfer?.items ?? [])]
    const isFolder = (i: DataTransferItem) => !!i.webkitGetAsEntry?.()?.isDirectory
    const files = items
      .filter((i) => i.kind === 'file' && !isFolder(i))
      .map((i) => i.getAsFile())
      .filter((f): f is File => !!f)
    transfers.send(files, items.filter(isFolder).length)
  }

  // A failed send has been dealt with once the menu that explains it closes.
  let explained = false
  $effect(() => {
    if (panelOpen) {
      if (transfers.failure) explained = true
    } else if (explained) {
      explained = false
      transfers.failure = null
    }
  })

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
        if (s === 'connected') sendShared()
      },
      onStream(stream) {
        video.srcObject = stream
        video.play().catch(() => {})
      },
      onAudio(stream) {
        speaker.srcObject = stream
        if (sound) playSound()
      },
      onMessage(msg) {
        if (msg.t === 'displays') {
          // The picture is back, so whatever went wrong with it is over.
          error = null
          displays = msg.displays
          activeDisplay = msg.active
          if (msg.window) restoring = null
          // Until the window asked for after reconnecting is back, the menu keeps showing it.
          if (!restoring) {
            if (shownWindow && !msg.window) windowGone()
            shownWindow = msg.window ?? null
          }
          session.options.display = msg.active
        } else if (msg.t === 'windows') {
          windows = msg.windows
          // The host lists the windows instead when the one asked for has closed.
          if (restoring) {
            restoring = null
            shownWindow = null
            windowGone()
          }
        } else if (msg.t === 'error') {
          error = new Problem(`Couldn’t show the screen of ${info.name}. Try another display, or reconnect.`, msg.message)
        } else if (msg.t === 'clipboard') {
          void copyHere(msg.text)
        } else if (msg.t === 'cursor') {
          cursor = msg.name
        } else if (msg.t === 'route') {
          onRoute(msg.route)
        } else if (msg.t === 'filesCopied') {
          transfers.hostCopied(msg.offer, msg.files, msg.folders)
        }
      },
      onOpen() {
        session.send({ t: 'setAudio', enabled: sound })
        session.send({ t: 'hideScreen', hide: hideScreen })
        // After reconnecting, the same window as before.
        if (shownWindow) {
          restoring = shownWindow.id
          session.send({ t: 'selectWindow', id: shownWindow.id })
        }
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
    const detachDesktop = attachDesktopInput(surface, video, send, {
      localShortcut,
      onPaste: shareClipboard,
      pen: !!info.pen,
    })
    const detachTouch = attachTouchInput(surface, video, pointer, send, {
      displayWidth: () =>
        shownWindow?.width || displays.find((d) => d.id === activeDisplay)?.width || video.videoWidth || 1920,
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
    const onShare = () => phase === 'connected' && sendShared()
    document.addEventListener('visibilitychange', onVisibility)
    document.addEventListener('fullscreenchange', onFullscreen)
    window.addEventListener('focus', onFocus)
    window.addEventListener('conductorshare', onShare)
    return () => {
      document.removeEventListener('visibilitychange', onVisibility)
      document.removeEventListener('fullscreenchange', onFullscreen)
      window.removeEventListener('focus', onFocus)
      window.removeEventListener('conductorshare', onShare)
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

  function savedSound() {
    try {
      return localStorage.getItem(SOUND_KEY) !== 'off'
    } catch {
      return true
    }
  }

  function toggleSound() {
    sound = !sound
    try {
      localStorage.setItem(SOUND_KEY, sound ? 'on' : 'off')
    } catch {
      // Not remembered; it still changed.
    }
    session.send({ t: 'setAudio', enabled: sound })
    if (sound) playSound()
    else soundLater?.abort()
  }

  function savedHideScreen() {
    try {
      return localStorage.getItem(HIDE_KEY) === 'on'
    } catch {
      return false
    }
  }

  function toggleHideScreen() {
    hideScreen = !hideScreen
    try {
      if (hideScreen) localStorage.setItem(HIDE_KEY, 'on')
      else localStorage.removeItem(HIDE_KEY)
    } catch {
      // Not remembered; it still changed.
    }
    session.send({ t: 'hideScreen', hide: hideScreen })
  }

  // A browser may not play sound until the page is used, so then it starts on the next tap, click or key.
  function playSound() {
    soundLater?.abort()
    // Until the sound arrives; then this runs again.
    if (!speaker.srcObject) return
    speaker.play().catch(() => {
      const later = new AbortController()
      soundLater = later
      const retry = () => {
        later.abort()
        if (sound) playSound()
      }
      // A touch counts as using the page when it ends.
      window.addEventListener('pointerup', retry, { capture: true, signal: later.signal })
      window.addEventListener('keydown', retry, { capture: true, signal: later.signal })
    })
  }

  function onRoute(next: Route) {
    const wasRelayed = !!route && route.kind !== 'direct'
    route = next
    if (next.kind === 'direct') {
      clearTimeout(relayTimer)
      relayedNote = false
    } else if (!wasRelayed) {
      clearTimeout(relayTimer)
      relayTimer = window.setTimeout(() => {
        relayedNote = true
        relayTimer = window.setTimeout(() => (relayedNote = false), 4000)
      }, 6000)
    }
  }

  function routeText(r: Route) {
    if (r.kind === 'direct') return 'Direct connection'
    if (r.kind === 'peerRelay') return 'Relayed by another device'
    return r.region ? `Relayed by Tailscale (${r.region})` : 'Relayed by Tailscale'
  }

  function selectDisplay(id: string) {
    if (id === activeDisplay && !shownWindow) return
    activeDisplay = id
    shownWindow = null
    session.options.display = id
    session.send({ t: 'selectDisplay', id })
    panelOpen = false
  }

  function windowGone() {
    windowGoneNote = true
    clearTimeout(windowGoneTimer)
    windowGoneTimer = window.setTimeout(() => (windowGoneNote = false), 4000)
  }

  function listWindows() {
    windows = null
    session.send({ t: 'listWindows' })
  }

  function selectWindow(id: string) {
    panelOpen = false
    if (id !== shownWindow?.id) session.send({ t: 'selectWindow', id })
  }

  function atLeast(version: string, least: number[]) {
    const parts = version.split('.').map((p) => parseInt(p, 10) || 0)
    for (const [i, n] of least.entries()) {
      if ((parts[i] ?? 0) !== n) return (parts[i] ?? 0) > n
    }
    return true
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

  onDestroy(() => {
    clearTimeout(relayTimer)
    clearTimeout(windowGoneTimer)
    soundLater?.abort()
    transfers.stop()
    session.close()
  })

  const quality = $derived(
    phase === 'reconnecting' ? 'bad' : stats && stats.rttMs != null && stats.rttMs > 80 ? 'poor' : 'good',
  )

  // What the handle says, most important first.
  const notice = $derived.by(() => {
    if (hasVideo && error) return { chip: 'Can’t show this display', problem: error, warning: true }
    if (transfers.notice) return transfers.notice
    if (windowGoneNote) return { chip: 'Window closed or minimized' }
    return relayedNote ? { chip: 'Relayed connection' } : null
  })
</script>

<div
  class="stream"
  style:height={viewHeight ? `${viewHeight}px` : null}
  role="presentation"
  ondragenter={onDragEnter}
  ondragover={onDragOver}
  ondragleave={onDragLeave}
  ondrop={onDrop}
>
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

  <audio bind:this={speaker} muted={!sound}></audio>

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

  <input
    bind:this={picker}
    type="file"
    multiple
    hidden
    onchange={() => {
      transfers.send([...(picker.files ?? [])])
      picker.value = ''
    }}
  />

  {#if dragging}
    <div class="drop">
      <p>Drop to send to the Downloads folder on {info.name}</p>
    </div>
  {/if}

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
      {#if route}<span>{routeText(route)}</span>{/if}
      {#if stats.decoder}<span>Decoder: {stats.decoder}</span>{/if}
      {#if stats.dropped || stats.lost}<span>{stats.dropped} frames dropped, {stats.lost} packets lost</span>{/if}
    </div>
  {/if}

  {#if phase !== 'failed'}
    <Handle
      bind:open={panelOpen}
      {quality}
      reconnecting={phase === 'reconnecting'}
      {notice}
      sending={!!transfers.sending}
      saving={!!transfers.saving}
      copiedFiles={transfers.copied?.files.length ?? 0}
      {displays}
      {activeDisplay}
      {shownWindow}
      {windows}
      {fullscreen}
      {showStats}
      {sound}
      pc={info.name}
      {hideScreen}
      onselectdisplay={selectDisplay}
      onlistwindows={canShowWindows ? listWindows : undefined}
      onselectwindow={selectWindow}
      ontogglefullscreen={toggleFullscreen}
      ontogglestats={() => (showStats = !showStats)}
      ontogglesound={toggleSound}
      ontogglehidescreen={toggleHideScreen}
      onkeyboard={coarse ? () => keyboard.show() : undefined}
      onsendfiles={() => picker.click()}
      onstopsending={() => transfers.stopSending()}
      onsavecopied={() => transfers.save()}
      onstopsaving={() => transfers.stopSaving()}
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

  .drop {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: var(--space-6);
    background: var(--overlay-scrim);
    pointer-events: none;
  }

  .drop p {
    margin: 0;
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius);
    background: var(--overlay-solid);
    text-align: center;
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
