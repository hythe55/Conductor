<script lang="ts">
  import { tick } from 'svelte'
  import Icon from './Icon.svelte'
  import ProblemText from './ProblemText.svelte'
  import type { Problem } from '../lib/errors'
  import type { DisplayInfo } from '../lib/types'

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
    displays: DisplayInfo[]
    activeDisplay: string
    fullscreen: boolean
    showStats: boolean
    onselectdisplay: (id: string) => void
    ontogglefullscreen: () => void
    ontogglestats: () => void
    /** Opens the on-screen keyboard. Only given where there's no real one. */
    onkeyboard?: () => void
    onsendfiles: () => void
    onstopsending: () => void
    ondisconnect: () => void
  }

  let {
    open = $bindable(false),
    quality,
    reconnecting,
    notice,
    sending,
    displays,
    activeDisplay,
    fullscreen,
    showStats,
    onselectdisplay,
    ontogglefullscreen,
    ontogglestats,
    onkeyboard,
    onsendfiles,
    onstopsending,
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

  function wake() {
    awake = true
    clearTimeout(timer)
    timer = window.setTimeout(() => (awake = false), 2500)
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

  // Focus moves into the menu when it opens. The session view takes it back when the menu closes.
  $effect(() => {
    if (open) tick().then(() => panel?.querySelector<HTMLElement>('[role^="menuitem"]')?.focus())
  })

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

  // Show the handle when something new is going on, so the chip is seen.
  $effect(() => {
    if (notice) wake()
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
  class:visible={awake || open || reconnecting || !!problem || sending}
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
    onfocus={wake}
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
    class:checks={displays.length > 1}
    style:left="{panelLeft}px"
    onkeydown={onPanelKey}
  >
    {#if problem}
      <div class="problem" role="alert"><ProblemText {problem} /></div>
    {/if}
    <div role="menu" aria-label="Session">
      {#if displays.length > 1}
        <div class="group" role="group" aria-label="Display">
          {#each displays as d, i (d.id)}
            <button
              class="item"
              role="menuitemradio"
              aria-checked={d.id === activeDisplay}
              tabindex="-1"
              onclick={() => run(() => onselectdisplay(d.id))}
            >
              <span class="check">{#if d.id === activeDisplay}<Icon name="check" />{/if}</span>
              <span class="text">{displayLabel(d, i)}</span>
            </button>
          {/each}
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
            <span class="text">Stop sending files</span>
          </button>
        {/if}
        {#if canFullscreen}
          <button class="item" role="menuitem" tabindex="-1" onclick={() => run(ontogglefullscreen)}>
            <span class="check"></span>
            <span class="text">{fullscreen ? 'Exit full screen' : 'Full screen'}</span>
            {#if !coarse}<kbd>Ctrl+Alt+Shift+F</kbd>{/if}
          </button>
        {/if}
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
    max-width: calc(100vw - 2 * var(--space-4));
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

  .item {
    display: grid;
    grid-template-columns: 1fr auto;
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

  .panel.checks .item {
    grid-template-columns: 16px 1fr auto;
  }

  .panel:not(.checks) .check {
    display: none;
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
