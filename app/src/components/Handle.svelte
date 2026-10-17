<script lang="ts">
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
    ondisconnect,
  }: Props = $props()

  const coarse = window.matchMedia('(pointer: coarse)').matches
  const canFullscreen = document.fullscreenEnabled

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

  $effect(() => {
    if (!open) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        open = false
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  })

  function run(action: () => void) {
    open = false
    action()
  }

  function displayLabel(d: DisplayInfo, index: number) {
    const name = d.name || `Display ${index + 1}`
    return `${name}, ${d.width}×${d.height}`
  }
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
    aria-label={reconnecting ? 'Reconnecting. Open session menu' : 'Open session menu'}
    onclick={() => (open = !open)}
    onfocus={wake}
  >
    {#if reconnecting}
      <span class="label">Reconnecting…</span>
    {:else}
      <span class="grip" aria-hidden="true"></span>
      {#if quality !== 'good'}<span class="dot {quality}" aria-hidden="true"></span>{/if}
    {/if}
  </button>

  {#if open}
    <div class="panel" class:checks={displays.length > 1} role="menu">
      {#if displays.length > 1}
        <div class="group" role="group" aria-label="Display">
          {#each displays as d, i (d.id)}
            <button
              class="item"
              role="menuitemradio"
              aria-checked={d.id === activeDisplay}
              onclick={() => run(() => onselectdisplay(d.id))}
            >
              <span class="check" aria-hidden="true">{d.id === activeDisplay ? '✓' : ''}</span>
              <span class="text">{displayLabel(d, i)}</span>
            </button>
          {/each}
        </div>
      {/if}

      <div class="group">
        {#if canFullscreen}
          <button class="item" role="menuitem" onclick={() => run(ontogglefullscreen)}>
            <span class="check" aria-hidden="true"></span>
            <span class="text">{fullscreen ? 'Exit full screen' : 'Full screen'}</span>
            {#if !coarse}<kbd>Ctrl+Alt+Shift+F</kbd>{/if}
          </button>
        {/if}
        <button class="item" role="menuitem" onclick={() => run(ontogglestats)}>
          <span class="check" aria-hidden="true"></span>
          <span class="text">{showStats ? 'Hide connection details' : 'Show connection details'}</span>
          {#if !coarse}<kbd>Ctrl+Alt+Shift+S</kbd>{/if}
        </button>
      </div>

      <div class="group">
        <button class="item" role="menuitem" onclick={() => run(ondisconnect)}>
          <span class="check" aria-hidden="true"></span>
          <span class="text">Disconnect</span>
          {#if !coarse}<kbd>Ctrl+Alt+Shift+Q</kbd>{/if}
        </button>
      </div>

      {#if !coarse}
        <p class="hint">Ctrl+Alt+Shift+M opens this menu.</p>
      {/if}
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
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--space-2);
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
    background: rgb(48 48 48 / 95%);
  }

  .grip {
    width: 24px;
    height: 3px;
    border-radius: 2px;
    background: var(--overlay-muted);
  }

  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
  }

  .dot.poor {
    background: var(--status-asleep);
  }

  .dot.bad {
    background: var(--status-error);
  }

  .label {
    font-size: 12px;
  }

  .panel {
    margin-top: var(--space-2);
    min-width: 260px;
    max-width: calc(100vw - 2 * var(--space-4));
    padding: var(--space-1);
    border-radius: var(--radius);
    background: var(--overlay-solid);
    color: var(--overlay-text);
    box-shadow: 0 8px 24px rgb(0 0 0 / 40%);
  }

  .group + .group {
    margin-top: var(--space-1);
    padding-top: var(--space-1);
    border-top: 1px solid rgb(255 255 255 / 10%);
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
    background: rgb(255 255 255 / 8%);
  }

  .panel.checks .item {
    grid-template-columns: 16px 1fr auto;
  }

  .panel:not(.checks) .check {
    display: none;
  }

  .check {
    font-size: 12px;
    color: var(--accent);
  }

  .text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  kbd {
    font: inherit;
    font-size: 12px;
    color: var(--overlay-muted);
  }

  .hint {
    margin: var(--space-1) 0 0;
    padding: var(--space-2) var(--space-3) var(--space-1);
    border-top: 1px solid rgb(255 255 255 / 10%);
    font-size: 12px;
    color: var(--overlay-muted);
  }

  @media (pointer: coarse) {
    .handle {
      height: 28px;
      min-width: 80px;
    }

    .item {
      min-height: 44px;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .handle-area {
      transition: none;
    }
  }
</style>
