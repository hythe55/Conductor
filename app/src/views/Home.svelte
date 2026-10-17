<script lang="ts">
  import { hostInfo } from '../lib/api'
  import type { HostInfo } from '../lib/types'

  let { onconnect }: { onconnect: (t: { url: string; info: HostInfo }) => void } = $props()

  let info = $state<HostInfo | null>(null)
  let error = $state<string | null>(null)

  async function load() {
    error = null
    try {
      info = await hostInfo('')
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
  }

  load()

  function describe(i: HostInfo) {
    const n = i.displays.length
    const os = i.os === 'windows' ? 'Windows' : i.os
    return `${os}, ${n} ${n === 1 ? 'display' : 'displays'}`
  }
</script>

<main>
  <h1>Devices</h1>

  {#if error}
    <div class="problem" role="alert">
      <p>{error}</p>
      <button class="btn" onclick={load}>Try again</button>
    </div>
  {:else if info}
    <ul class="devices">
      <li>
        <span class="status online" aria-label="Online"></span>
        <div class="device">
          <span class="name">{info.name}</span>
          <span class="meta">{describe(info)}</span>
        </div>
        <button class="btn primary" onclick={() => onconnect({ url: '', info: info! })}>Connect</button>
      </li>
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
    padding: var(--space-4) 0;
    border-bottom: 1px solid var(--border);
  }

  .status {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--status-offline);
  }

  .status.online {
    background: var(--status-online);
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
