<script lang="ts">
  import { tick } from 'svelte'
  import Icon from './Icon.svelte'
  import type { DisplayInfo } from '../lib/types'

  interface Props {
    open: boolean
    quality: 'good' | 'poor' | 'bad'
    reconnecting: boolean
    displays: DisplayInfo[]
    activeDisplay: string
    fullscreen: boolean
    showStats: boolean
    onselectdisplay: (id: string) => void
    ontogglefullscreen: () => void
    ontogglestats: () => void
    /** Opens the on-screen keyboard. Only given where there's no real one. */
    onkeyboard?: () => void
    ondisconnect: () => void
  }

  let {
    open = $bindable(false),
    quality,
    reconnecting,
    displays,
    activeDisplay,
    fullscreen,
    showStats,
    onselectdisplay,
    ontogglefullscreen,
    ontogglestats,
    onkeyboard,
    ondisconnect,
  }: Props = $props()

  const coarse = window.matchMedia('(pointer: coarse)').matches
  const canFullscreen = document.fullscreenEnabled

  let panel = $state<HTMLDivElement>()

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
    if (open) tick().then(() => items()[0]?.focus())
  })

  function items(): HTMLElement[] {
    return panel ? Array.from(panel.querySelectorAll<HTMLElement>('[role^="menuitem"]')) : []
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

  const chip = $derived(reconnecting ? 'Reconnecting…' : quality === 'poor' ? 'Slow connection' : null)
</script>

{#if open}
  <!-- Catches the click that closes the menu so it never reaches the remote PC. -->
  <div class="backdrop" role="presentation" onpointerdown={() => (open = false)}></div>
{/if}

<div class="handle-area" class:visible={awake || open || reconnecting || coarse}>
  <button
    class="handle"
    class:open
    aria-expanded={open}
    aria-haspopup="menu"
    aria-label={chip ? `${chip} Session menu` : 'Session menu'}
    title={coarse ? undefined : 'Session menu (Ctrl+Alt+Shift+M)'}
    onclick={() => (open = !open)}
    onfocus={wake}
  >
    {#if chip}
      <span class="chip">{chip}</span>
    {:else}
      <span class="grip" aria-hidden="true"></span>
    {/if}
  </button>

  {#if open}
    <!-- svelte-ignore a11y_interactive_supports_focus -->
    <div
      bind:this={panel}
      class="panel"
      class:checks={displays.length > 1}
      role="menu"
      aria-label="Session"
      onkeydown={onPanelKey}
    >
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
  {/if}
</div>

<style>
  .backdrop {
    position: absolute;
    inset: 0;
  }

  .handle-area {
    position: absolute;
    top: 0;
    left: 50%;
    transform: translateX(-50%);
    display: flex;
    flex-direction: column;
    align-items: center;
    padding-top: env(safe-area-inset-top);
    opacity: 0;
    transition: opacity 200ms ease;
  }

  .handle-area.visible,
  .handle-area:focus-within {
    opacity: 1;
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
    font-size: var(--text-sm);
    color: var(--overlay-warning);
  }

  .panel {
    margin-top: var(--space-2);
    min-width: 260px;
    max-width: calc(100vw - 2 * var(--space-4));
    padding: var(--space-1);
    border: 1px solid var(--overlay-border);
    border-radius: var(--radius);
    background: var(--overlay-solid);
    color: var(--overlay-text);
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
    color: var(--overlay-accent);
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
