<script lang="ts">
  import { getVersion } from '@tauri-apps/api/app'
  import Icon from '../components/Icon.svelte'
  import ProblemText from '../components/ProblemText.svelte'
  import Switch from '../components/Switch.svelte'
  import { type Problem, toProblem } from '../lib/errors'
  import { hostStatus, setHosting } from '../lib/platform'
  import type { ControlStatus } from '../lib/types'

  let { onback }: { onback: () => void } = $props()

  let status = $state<ControlStatus | null>(null)
  let loaded = $state(false)
  let busy = $state(false)
  let error = $state<Problem | null>(null)
  let version = $state('')

  async function refresh() {
    try {
      status = await hostStatus()
      error = null
    } catch (e) {
      error = toProblem(e, 'Couldn’t check whether this PC can be controlled.')
    }
    loaded = true
  }

  $effect(() => {
    refresh()
    getVersion().then((v) => (version = v)).catch(() => {})
    // The agent takes a moment to start after hosting is turned on. Nothing is polled while hidden.
    let timer = window.setInterval(refresh, 3000)
    const onVisibility = () => {
      clearInterval(timer)
      if (document.hidden) return
      refresh()
      timer = window.setInterval(refresh, 3000)
    }
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      clearInterval(timer)
      document.removeEventListener('visibilitychange', onVisibility)
    }
  })

  async function toggle(enabled: boolean) {
    busy = true
    error = null
    try {
      status = await setHosting(enabled)
    } catch (e) {
      error = toProblem(e, enabled ? 'Couldn’t turn this on.' : 'Couldn’t turn this off.')
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
  <button class="link" aria-label="Back to devices" onclick={onback}><Icon name="back" />Devices</button>
  <h1>Settings</h1>

  <section aria-labelledby="this-pc">
    <h2 id="this-pc">This PC</h2>
    <div class="group">
      <div class="row">
        <div class="text">
          <span class="label" id="hosting-label">Let my other devices control this PC</span>
          <span class="note">{hostingNote}</span>
          {#if error}<div class="note error" role="alert"><ProblemText problem={error} /></div>{/if}
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
    padding: var(--space-6);
    padding-top: max(var(--space-6), env(safe-area-inset-top));
  }

  h1 {
    margin: var(--space-2) 0 var(--space-6);
    font-family: var(--font-display);
    font-size: var(--text-lg);
    font-weight: 600;
  }

  section + section {
    margin-top: var(--space-8);
  }

  h2 {
    margin: 0 0 var(--space-2);
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--text-muted);
  }

  .group {
    border-top: 1px solid var(--border);
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    padding: var(--space-3) 0;
    border-bottom: 1px solid var(--border);
  }

  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .note {
    max-width: 72ch;
    color: var(--text-muted);
    font-size: var(--text-sm);
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
