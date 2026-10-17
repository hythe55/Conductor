<script lang="ts">
  import { getVersion } from '@tauri-apps/api/app'
  import Switch from '../components/Switch.svelte'
  import { hostStatus, setHosting } from '../lib/platform'
  import type { ControlStatus } from '../lib/types'

  let { onback }: { onback: () => void } = $props()

  let status = $state<ControlStatus | null>(null)
  let loaded = $state(false)
  let busy = $state(false)
  let error = $state<string | null>(null)
  let version = $state('')

  async function refresh() {
    try {
      status = await hostStatus()
      error = null
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
    loaded = true
  }

  $effect(() => {
    refresh()
    getVersion().then((v) => (version = v)).catch(() => {})
    // The agent takes a moment to start after hosting is turned on.
    const timer = window.setInterval(refresh, 3000)
    return () => clearInterval(timer)
  })

  async function toggle(enabled: boolean) {
    busy = true
    error = null
    try {
      status = await setHosting(enabled)
    } catch (e) {
      error = e instanceof Error ? e.message : String(e)
    }
    busy = false
  }

  const hostingNote = $derived.by(() => {
    if (!loaded) return ''
    if (!status) return 'The Conductor service isn’t installed on this PC. Reinstall Conductor to turn this on.'
    if (!status.hosting) return 'Off. Other devices can’t see or control this PC.'
    if (!status.agentRunning) return 'Starting…'
    return 'On. Your devices on Tailscale can see this screen and use its keyboard and mouse, including at the sign-in screen.'
  })
</script>

<main>
  <button class="back" onclick={onback}>Devices</button>
  <h1>Settings</h1>

  <section aria-labelledby="this-pc">
    <h2 id="this-pc">This PC</h2>
    <div class="group">
      <div class="row">
        <div class="text">
          <span class="label" id="hosting-label">Let my other devices control this PC</span>
          <span class="note">{hostingNote}</span>
          {#if error}<span class="note error" role="alert">{error}</span>{/if}
        </div>
        <Switch
          checked={status?.hosting ?? false}
          disabled={busy || !status}
          labelledby="hosting-label"
          onchange={toggle}
        />
      </div>
    </div>
  </section>

  <section aria-labelledby="about">
    <h2 id="about">About</h2>
    <div class="group">
      <div class="row">
        <div class="text">
          <span class="label">Conductor {version}</span>
          <span class="note">Free and open source under the GPL-3.0 license.</span>
        </div>
      </div>
    </div>
  </section>
</main>

<style>
  main {
    max-width: 760px;
    margin: 0 auto;
    padding: var(--space-6);
    padding-top: max(var(--space-6), env(safe-area-inset-top));
  }

  .back {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-1) 0;
    border: 0;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
  }

  .back::before {
    content: '‹';
    font-size: 18px;
    line-height: 1;
  }

  .back:hover {
    color: var(--text);
  }

  h1 {
    margin: var(--space-2) 0 var(--space-6);
    font-family: var(--font-display);
    font-size: 26px;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  section + section {
    margin-top: var(--space-6);
  }

  h2 {
    margin: 0 0 var(--space-2);
    font-size: 14px;
    font-weight: 600;
  }

  .group {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    padding: var(--space-4);
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .note {
    max-width: 60ch;
    color: var(--text-muted);
    font-size: 13px;
  }

  .note.error {
    color: var(--status-error);
  }

  @media (max-width: 480px) {
    main {
      padding-inline: var(--space-4);
    }
  }
</style>
