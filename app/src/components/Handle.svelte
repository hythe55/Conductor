<script lang="ts">
  import { tick } from 'svelte'
  import Icon from './Icon.svelte'
  import ProblemText from './ProblemText.svelte'
  import type { Problem } from '../lib/errors'
  import type { DisplayInfo, ShownWindow, WindowInfo } from '../lib/types'

  /** Status shown on the handle; a problem is explained at the top of the menu. */
  interface Notice {
    chip: string
    /** Percent done, shown after the chip but not announced each time it changes. */
    progress?: number
    problem?: Problem
    warning?: boolean
  }

  interface Props {
    open: boolean
    quality: 'good' | 'poor' | 'bad'
    reconnecting: boolean
    notice: Notice | null
    /** Files are on their way to the remote PC. */
    sending: boolean
    /** Files copied on the remote PC are being saved here. */
    saving: boolean
    /** How many files copied on the remote PC can be saved here. */
    copiedFiles: number
    displays: DisplayInfo[]
    activeDisplay: string
    /** The one window shown instead of the whole display. */
    shownWindow: ShownWindow | null
    /** The windows that could be shown, once listed. */
    windows: WindowInfo[] | null
    fullscreen: boolean
    showStats: boolean
    sound: boolean
    /** The remote PC's name. */
    pc: string
    hideScreen: boolean
    onselectdisplay: (id: string) => void
    /** Asks for the windows open on the remote PC. Only given where it can show one. */
    onlistwindows?: () => void
    onselectwindow: (id: string) => void
    ontogglefullscreen: () => void
    ontogglestats: () => void
    ontogglesound: () => void
    ontogglehidescreen: () => void
    /** Opens the on-screen keyboard. Only given where there's no real one. */
    onkeyboard?: () => void
    onsendfiles: () => void
    onstopsending: () => void
    onsavecopied: () => void
    onstopsaving: () => void
    ondisconnect: () => void
  }

  let {
    open = $bindable(false),
    quality,
    reconnecting,
    notice,
    sending,
    saving,
    copiedFiles,
    displays,
    activeDisplay,
    shownWindow,
    windows,
    fullscreen,
    showStats,
    sound,
    pc,
    hideScreen,
    onselectdisplay,
    onlistwindows,
    onselectwindow,
    ontogglefullscreen,
    ontogglestats,
    ontogglesound,
    ontogglehidescreen,
    onkeyboard,
    onsendfiles,
    onstopsending,
    onsavecopied,
    onstopsaving,
    ondisconnect,
  }: Props = $props()

  const coarse = window.matchMedia('(pointer: coarse)').matches
  const canFullscreen = document.fullscreenEnabled

  let panel = $state<HTMLDivElement>()
  let panelWidth = $state(0)
  let handleWidth = $state(0)
  let windowWidth = $state(window.innerWidth)

  // Where the handle sits along the top edge, as a share of the width. It can be
  // dragged aside when it covers something, and stays there next time.
  const PLACE_KEY = 'conductor.handle'
  let place = $state(savedPlace())
  let drag: { id: number; startX: number; start: number; moved: boolean } | null = null
  let dragged = false

  function savedPlace() {
    try {
      const v = Number(localStorage.getItem(PLACE_KEY))
      return v > 0 && v < 1 ? v : 0.5
    } catch {
      return 0.5
    }
  }

  function onHandleDown(e: PointerEvent) {
    if (e.button !== 0) return
    drag = { id: e.pointerId, startX: e.clientX, start: place, moved: false }
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
  }

  function onHandleMove(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.id) return
    const dx = e.clientX - drag.startX
    if (!drag.moved && Math.abs(dx) < 6) return
    drag.moved = true
    place = Math.min(0.95, Math.max(0.05, drag.start + dx / window.innerWidth))
    wake()
  }

  function onHandleUp(e: PointerEvent) {
    if (!drag || e.pointerId !== drag.id) return
    if (drag.moved) {
      // The click that ends a drag doesn't open the menu.
      dragged = true
      try {
        localStorage.setItem(PLACE_KEY, String(place))
      } catch {
        // Not remembered; it still moved.
      }
    }
    drag = null
  }

  function onHandleClick() {
    if (dragged) {
      dragged = false
      return
    }
    open = !open
  }

  // The handle and its menu stay inside the window, wherever the handle was left.
  const handleCenter = $derived(
    Math.min(Math.max(handleWidth / 2, place * windowWidth), windowWidth - handleWidth / 2),
  )
  const panelLeft = $derived(
    Math.min(Math.max(8, handleCenter - panelWidth / 2), windowWidth - panelWidth - 8),
  )

  // The handle fades out while you work and comes back when the pointer nears the top edge.
  let awake = $state(true)
  let timer: number | undefined

  function wake(ms = 2500) {
    awake = true
    clearTimeout(timer)
    timer = window.setTimeout(() => (awake = false), ms)
  }

  $effect(() => {
    wake()
    const onMove = (e: PointerEvent) => {
      if (e.clientY < 72) wake()
    }
    window.addEventListener('pointermove', onMove)
    return () => {
      window.removeEventListener('pointermove', onMove)
      clearTimeout(timer)
    }
  })

  // Show the handle for a moment when the connection gets slow, so the chip is seen.
  $effect(() => {
    if (quality === 'poor') wake()
  })

  // The menu, or the list of windows that replaces it at the same width.
  let view = $state<'menu' | 'windows'>('menu')
  let menuWidth = $state(0)

  // Focus moves into the menu when it opens. The session view takes it back when the menu closes.
  $effect(() => {
    if (open) tick().then(() => panel?.querySelector<HTMLElement>('[role^="menuitem"]')?.focus())
    else view = 'menu'
  })

  async function showView(next: 'menu' | 'windows') {
    if (next === 'windows') menuWidth = panelWidth
    view = next
    if (next === 'windows') onlistwindows?.()
    await tick()
    const to = next === 'menu' ? panel?.querySelector<HTMLElement>('#show-window') : null
    ;(to ?? panel?.querySelector<HTMLElement>('[role^="menuitem"]'))?.focus()
  }

  // The arrow keys also reach the details of a problem shown above the menu.
  function items(): HTMLElement[] {
    return panel ? Array.from(panel.querySelectorAll<HTMLElement>('[role^="menuitem"], summary')) : []
  }

  function onPanelKey(e: KeyboardEvent) {
    const list = items()
    const at = list.indexOf(document.activeElement as HTMLElement)
    let next: number
    switch (e.key) {
      case 'ArrowDown':
        next = (at + 1) % list.length
        break
      case 'ArrowUp':
        next = (at - 1 + list.length) % list.length
        break
      case 'Home':
        next = 0
        break
      case 'End':
        next = list.length - 1
        break
      case 'Escape':
        e.preventDefault()
        // From the list of windows, back to the menu.
        if (view === 'windows') void showView('menu')
        else open = false
        return
      case 'Tab':
        e.preventDefault()
        open = false
        return
      default:
        return
    }
    e.preventDefault()
    list[next]?.focus()
  }

  function run(action: () => void) {
    open = false
    action()
  }

  function displayLabel(d: DisplayInfo, index: number) {
    const name = d.name || `Display ${index + 1}`
    return `${name}, ${d.width}×${d.height}`
  }

  const status = $derived<Notice | null>(
    reconnecting
      ? { chip: 'Reconnecting…', warning: true }
      : (notice ?? (quality === 'poor' ? { chip: 'Slow connection', warning: true } : null)),
  )
  const problem = $derived(notice?.problem ?? null)

  // Show the handle when something new is going on, for as long as a passing note lasts.
  $effect(() => {
    if (notice) wake(4000)
  })
</script>

<svelte:window bind:innerWidth={windowWidth} />

{#if open}
  <!-- Catches the click that closes the menu so it never reaches the remote PC. -->
  <div class="backdrop" role="presentation" onpointerdown={() => (open = false)}></div>
{/if}

<!-- Touch screens can't bring the handle back by hovering, so there it dims instead of hiding. -->
<div
  class="handle-area"
  class:visible={awake || open || reconnecting || !!problem || sending || saving}
  class:coarse
  style:left="{handleCenter}px"
  bind:offsetWidth={handleWidth}
>
  <button
    class="handle"
    class:open
    aria-expanded={open}
    aria-haspopup="menu"
    aria-label="Session menu"
    aria-describedby="handle-status"
    title={coarse ? undefined : 'Session menu (Ctrl+Alt+Shift+M). Drag to move it.'}
    onpointerdown={onHandleDown}
    onpointermove={onHandleMove}
    onpointerup={onHandleUp}
    onpointercancel={onHandleUp}
    onclick={onHandleClick}
    onfocus={() => wake()}
  >
    {#if status}
      <span class="chip" class:warning={status.warning}
        >{status.chip}{#if status.progress != null}<span aria-hidden="true">{` ${status.progress}%`}</span>{/if}</span
      >
    {:else}
      <span class="grip" aria-hidden="true"></span>
    {/if}
  </button>
</div>
<span class="announce" id="handle-status" aria-live="polite">{status?.chip ?? ''}</span>

{#if open}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    bind:this={panel}
    bind:offsetWidth={panelWidth}
    class="panel"
    style:left="{panelLeft}px"
    style:width={view === 'windows' ? `${menuWidth}px` : null}
    onkeydown={onPanelKey}
  >
    {#if problem}
      <div class="problem" role="alert"><ProblemText {problem} /></div>
    {/if}
    {#if view === 'windows'}
      <div role="menu" aria-label="Windows on {pc}">
        <div class="group" role="group">
          <button class="item" role="menuitem" tabindex="-1" onclick={() => showView('menu')}>
            <span class="check"><Icon name="back" /></span>
            <span class="text">Back</span>
          </button>
        </div>
        {#if windows?.length}
          <div class="group windows" role="group" aria-label="Windows">
            {#each windows as w (w.id)}
              <button
                class="item"
                role="menuitemradio"
                aria-checked={w.id === shownWindow?.id}
                tabindex="-1"
                onclick={() => run(() => onselectwindow(w.id))}
              >
                <span class="check">{#if w.id === shownWindow?.id}<Icon name="check" />{/if}</span>
                <span class="text">{w.title}</span>
              </button>
            {/each}
          </div>
        {/if}
      </div>
      <!-- Empty, and hidden, while windows are listed. -->
      <p class="note" class:waiting={windows === null} role="status">{#if windows === null}Looking for windows…{:else if windows.length === 0}No windows are open on {pc}.{/if}</p>
    {:else}
      <div role="menu" aria-label="Session">
        {#if displays.length > 1 || shownWindow || onlistwindows}
          <div class="group" role="group" aria-label="Show">
            {#if displays.length > 1}
              {#each displays as d, i (d.id)}
                <button
                  class="item"
                  role="menuitemradio"
                  aria-checked={d.id === activeDisplay && !shownWindow}
                  tabindex="-1"
                  onclick={() => run(() => onselectdisplay(d.id))}
                >
                  <span class="check">{#if d.id === activeDisplay && !shownWindow}<Icon name="check" />{/if}</span>
                  <span class="text">{displayLabel(d, i)}</span>
                </button>
              {/each}
            {:else if shownWindow}
              <button
                class="item"
                role="menuitemradio"
                aria-checked="false"
                tabindex="-1"
                onclick={() => run(() => onselectdisplay(activeDisplay))}
              >
                <span class="check"></span>
                <span class="text">Whole screen</span>
              </button>
            {/if}
            {#if shownWindow}
              <button
                class="item"
                role="menuitemradio"
                aria-checked="true"
                tabindex="-1"
                onclick={() => run(() => onselectwindow(shownWindow!.id))}
              >
                <span class="check"><Icon name="check" /></span>
                <span class="text">{shownWindow.title}</span>
              </button>
            {/if}
            {#if onlistwindows}
              <button class="item" id="show-window" role="menuitem" tabindex="-1" onclick={() => showView('windows')}>
                <span class="check"></span>
                <span class="text">{shownWindow ? 'Show another window' : 'Show one window'}</span>
                <span class="more"><Icon name="forward" /></span>
              </button>
            {/if}
          </div>
        {/if}

        <div class="group" role="group">
          {#if onkeyboard}
            <button class="item" role="menuitem" tabindex="-1" onclick={() => run(onkeyboard)}>
              <span class="check"></span>
              <span class="text">Keyboard</span>
            </button>
          {/if}
          <button class="item" role="menuitem" tabindex="-1" onclick={() => run(onsendfiles)}>
            <span class="check"></span>
            <span class="text">{sending ? 'Send more files…' : 'Send files…'}</span>
          </button>
          {#if sending}
            <button class="item" role="menuitem" tabindex="-1" onclick={() => run(onstopsending)}>
              <span class="check"></span>
              <span class="text">Stop sending</span>
            </button>
          {/if}
          {#if saving}
            <button class="item" role="menuitem" tabindex="-1" onclick={() => run(onstopsaving)}>
              <span class="check"></span>
              <span class="text">Stop saving</span>
            </button>
          {:else if copiedFiles}
            <button class="item" role="menuitem" tabindex="-1" onclick={() => run(onsavecopied)}>
              <span class="check"></span>
              <span class="text">{copiedFiles === 1 ? 'Save copied file' : `Save ${copiedFiles} copied files`}</span>
            </button>
          {/if}
          {#if canFullscreen}
            <button class="item" role="menuitem" tabindex="-1" onclick={() => run(ontogglefullscreen)}>
              <span class="check"></span>
              <span class="text">{fullscreen ? 'Exit full screen' : 'Full screen'}</span>
              {#if !coarse}<kbd>Ctrl+Alt+Shift+F</kbd>{/if}
            </button>
          {/if}
          <button
            class="item"
            role="menuitemcheckbox"
            aria-checked={sound}
            tabindex="-1"
            onclick={() => run(ontogglesound)}
          >
            <span class="check">{#if sound}<Icon name="check" />{/if}</span>
            <span class="text">Play sound</span>
          </button>
          <button
            class="item"
            role="menuitemcheckbox"
            aria-checked={hideScreen}
            tabindex="-1"
            onclick={() => run(ontogglehidescreen)}
          >
            <span class="check">{#if hideScreen}<Icon name="check" />{/if}</span>
            <span class="text">Hide screen on {pc}</span>
          </button>
          <button class="item" role="menuitem" tabindex="-1" onclick={() => run(ontogglestats)}>
            <span class="check"></span>
            <span class="text">{showStats ? 'Hide connection details' : 'Show connection details'}</span>
            {#if !coarse}<kbd>Ctrl+Alt+Shift+S</kbd>{/if}
          </button>
        </div>

        <div class="group" role="group">
          <button class="item" role="menuitem" tabindex="-1" onclick={() => run(ondisconnect)}>
            <span class="check"></span>
            <span class="text">Disconnect</span>
            {#if !coarse}<kbd>Ctrl+Alt+Shift+Q</kbd>{/if}
          </button>
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .backdrop {
    position: absolute;
    inset: 0;
  }

  .handle-area {
    position: absolute;
    top: 0;
    transform: translateX(-50%);
    padding-top: env(safe-area-inset-top);
    white-space: nowrap;
    opacity: 0;
    transition: opacity 200ms ease;
  }

  .handle-area.coarse {
    opacity: 0.5;
  }

  .handle-area.visible,
  .handle-area:focus-within {
    opacity: 1;
  }

  .announce {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip-path: inset(50%);
    white-space: nowrap;
  }

  .handle {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: center;
    min-width: 64px;
    height: 22px;
    padding: 0 var(--space-3);
    border: 0;
    border-radius: 0 0 var(--radius-sm) var(--radius-sm);
    background: var(--overlay-bg);
    color: var(--overlay-text);
    cursor: pointer;
    touch-action: none;
  }

  .handle:hover,
  .handle.open {
    background: var(--overlay-bg-hover);
  }

  .grip {
    width: 24px;
    height: 3px;
    border-radius: 2px;
    background: var(--overlay-muted);
  }

  .chip {
    display: block;
    max-width: min(60vw, 360px);
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: var(--text-sm);
  }

  .chip.warning {
    color: var(--overlay-warning);
  }

  .panel {
    position: absolute;
    top: calc(env(safe-area-inset-top) + 22px + var(--space-2));
    min-width: 260px;
    max-width: min(400px, calc(100vw - 2 * var(--space-4)));
    padding: var(--space-1);
    border: 1px solid var(--overlay-border);
    border-radius: var(--radius);
    background: var(--overlay-solid);
    color: var(--overlay-text);
  }

  .problem {
    max-width: 320px;
    padding: var(--space-2) var(--space-3) var(--space-3);
    border-bottom: 1px solid var(--overlay-border);
    margin-bottom: var(--space-1);
    --details-color: var(--overlay-muted);
  }

  .problem :global(.message) {
    color: var(--overlay-warning);
  }

  .group + .group {
    margin-top: var(--space-1);
    padding-top: var(--space-1);
    border-top: 1px solid var(--overlay-border);
  }

  .windows {
    max-height: min(60vh, 420px);
    overflow-y: auto;
  }

  /* Long titles take two lines rather than losing their end. */
  .windows .item {
    padding-block: var(--space-1);
  }

  .windows .text {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    white-space: normal;
    overflow-wrap: anywhere;
  }

  .note {
    margin: var(--space-1) 0 0;
    padding: var(--space-2) var(--space-3);
    border-top: 1px solid var(--overlay-border);
    color: var(--overlay-muted);
  }

  .note:empty {
    display: none;
  }

  /* Shown only if the list takes a while. */
  .note.waiting {
    animation: show-late 300ms step-end both;
  }

  @keyframes show-late {
    from {
      visibility: hidden;
    }
  }

  .more {
    display: flex;
    color: var(--overlay-muted);
  }

  .item {
    display: grid;
    grid-template-columns: 16px 1fr auto;
    align-items: center;
    gap: var(--space-2);
    width: 100%;
    min-height: 34px;
    padding: 0 var(--space-3);
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: inherit;
    text-align: left;
    cursor: pointer;
  }

  .item:hover,
  .item:focus-visible {
    background: var(--overlay-hover);
  }

  .item:focus-visible {
    outline-offset: -2px;
  }

  .check {
    color: var(--overlay-text);
  }

  .text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  kbd {
    font: inherit;
    font-size: var(--text-sm);
    color: var(--overlay-muted);
  }

  @media (pointer: coarse) {
    .handle {
      height: 28px;
      min-width: 80px;
    }

    .panel {
      top: calc(env(safe-area-inset-top) + 28px + var(--space-2));
    }

    /* A finger-sized target without a bigger handle over the remote screen. */
    .handle::after {
      content: '';
      position: absolute;
      inset: -8px -16px -20px;
    }

    .item {
      min-height: var(--touch-target);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .handle-area {
      transition: none;
    }
  }
</style>
