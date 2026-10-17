<script lang="ts">
  import ProblemText from '../components/ProblemText.svelte'
  import { deviceUrl } from '../lib/api'
  import { type Problem, toProblem } from '../lib/errors'
  import { isDesktopApp, listDevices } from '../lib/platform'
  import type { Device, HostInfo } from '../lib/types'

  let {
    onconnect,
    onsettings,
  }: { onconnect: (t: { url: string; info: HostInfo }) => void; onsettings: () => void } = $props()

  let list = $state<Device[] | null>(null)
  let error = $state<Problem | null>(null)

  async function load() {
    try {
      list = (await listDevices()).filter((d) => !d.isYou)
      error = null
    } catch (e) {
      // Keep showing the last good list; only an empty screen needs the error.
      if (!list) error = toProblem(e, 'Couldn’t get the list of devices.')
    }
  }

  // Refresh while the list is on screen so devices that wake up or start
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
        return d.lastSeen ? `Offline, last seen ${ago(d.lastSeen)}` : 'Offline'
    }
  }

  const relative = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto' })

  function ago(iso: string) {
    const seconds = (Date.parse(iso) - Date.now()) / 1000
    if (!Number.isFinite(seconds) || seconds > -60) return 'just now'
    const units: [Intl.RelativeTimeFormatUnit, number][] = [
      ['minute', 60],
      ['hour', 60],
      ['day', 24],
      ['week', 7],
      ['month', 4.35],
      ['year', 12],
    ]
    let value = seconds / 60
    let unit: Intl.RelativeTimeFormatUnit = 'minute'
    for (let i = 1; i < units.length && Math.abs(value) >= units[i][1]; i++) {
      value /= units[i][1]
      unit = units[i][0]
    }
    return relative.format(Math.round(value), unit)
  }
</script>

<main>
  <header>
    <h1>Devices</h1>
    {#if isDesktopApp}
      <button class="link" onclick={onsettings}>Settings</button>
    {/if}
  </header>

  {#if error}
    <div class="problem" role="alert">
      <ProblemText problem={error} />
      <button class="btn" onclick={retry}>Try again</button>
    </div>
  {:else if list === null}
    <p class="quiet">Looking for your devices…</p>
  {:else if list.length === 0}
    <div class="problem">
      <p class="quiet">
        No other devices found. Install Conductor on a Windows PC that’s signed in to the same Tailscale account.
      </p>
      <button class="btn" onclick={retry}>Check again</button>
    </div>
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
            <button
              class="btn primary"
              aria-label="Connect to {d.name}"
              onclick={() => onconnect({ url: deviceUrl(d), info: d.host! })}
            >
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
    padding: var(--space-6);
    padding-top: max(var(--space-6), env(safe-area-inset-top));
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--space-2);
  }

  h1 {
    margin: 0;
    font-size: var(--text-md);
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
    min-height: 64px;
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
    font-size: var(--text-lg);
    font-weight: 600;
    line-height: 1.25;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .offline .name,
  .not-running .name {
    color: var(--text-muted);
  }

  .meta {
    font-size: var(--text-sm);
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
  }
</style>
