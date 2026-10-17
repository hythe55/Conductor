<script lang="ts">
  import { deviceUrl, devices as fetchDevices } from '../lib/api'
  import type { Device, HostInfo } from '../lib/types'

  let { onconnect }: { onconnect: (t: { url: string; info: HostInfo }) => void } = $props()

  let list = $state<Device[] | null>(null)
  let error = $state<string | null>(null)

  async function load() {
    try {
      list = (await fetchDevices('')).filter((d) => !d.isYou)
      error = null
    } catch (e) {
      // Keep showing the last good list; only an empty screen needs the error.
      if (!list) error = e instanceof Error ? e.message : String(e)
    }
  }

  // Refresh while the list is on screen so PCs that wake up or start
  // Conductor appear on their own. Nothing is polled while hidden.
  $effect(() => {
    load()
    let timer = window.setInterval(load, 10_000)
    const onVisibility = () => {
      clearInterval(timer)
      if (document.hidden) return
      load()
      timer = window.setInterval(load, 10_000)
    }
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      clearInterval(timer)
      document.removeEventListener('visibilitychange', onVisibility)
    }
  })

  function retry() {
    error = null
    list = null
    load()
  }

  function availability(d: Device): 'ready' | 'not-running' | 'offline' {
    if (d.host) return 'ready'
    return d.online ? 'not-running' : 'offline'
  }

  function describe(d: Device) {
    switch (availability(d)) {
      case 'ready': {
        const n = d.host!.displays.length
        return `${n} ${n === 1 ? 'display' : 'displays'}`
      }
      case 'not-running':
        return 'Conductor isn’t running'
      case 'offline':
        return 'Offline'
    }
  }
</script>

<main>
  <h1>Devices</h1>

  {#if error}
    <div class="problem" role="alert">
      <p>{error}</p>
      <button class="btn" onclick={retry}>Try again</button>
    </div>
  {:else if list === null}
    <p class="quiet">Looking for your devices…</p>
  {:else if list.length === 0}
    <p class="quiet">
      No other PCs found. Install Conductor on a Windows PC that’s signed in to the same Tailscale account.
    </p>
  {:else}
    <ul class="devices">
      {#each list as d (d.id)}
        {@const s = availability(d)}
        <li class={s}>
          <span class="status" aria-hidden="true"></span>
          <div class="device">
            <span class="name">{d.name}</span>
            <span class="meta">{describe(d)}</span>
          </div>
          {#if s === 'ready'}
            <button class="btn primary" onclick={() => onconnect({ url: deviceUrl(d), info: d.host! })}>
              Connect
            </button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</main>

<style>
  main {
    max-width: 760px;
    margin: 0 auto;
    padding: var(--space-8) var(--space-6);
    padding-top: max(var(--space-8), env(safe-area-inset-top));
  }

  h1 {
    margin: 0 0 var(--space-4);
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text-muted);
  }

  .quiet {
    margin: 0;
    max-width: 56ch;
    color: var(--text-muted);
  }

  .devices {
    list-style: none;
    margin: 0;
    padding: 0;
    border-top: 1px solid var(--border);
  }

  li {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: var(--space-4);
    min-height: 76px;
    padding: var(--space-3) 0;
    border-bottom: 1px solid var(--border);
  }

  .status {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--status-offline);
  }

  .ready .status {
    background: var(--status-online);
  }

  .not-running .status {
    background: transparent;
    box-shadow: inset 0 0 0 2px var(--status-offline);
  }

  .device {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  .name {
    font-family: var(--font-display);
    font-size: 26px;
    font-weight: 600;
    line-height: 1.15;
    letter-spacing: -0.01em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .offline .name,
  .not-running .name {
    color: var(--text-muted);
  }

  .meta {
    color: var(--text-muted);
  }

  .problem {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-3);
    max-width: 52ch;
  }

  .problem p {
    margin: 0;
  }

  @media (max-width: 480px) {
    main {
      padding-inline: var(--space-4);
    }

    .name {
      font-size: 22px;
    }
  }
</style>
