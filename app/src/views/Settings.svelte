<script lang="ts">
  import { getVersion } from '@tauri-apps/api/app'
  import Icon from '../components/Icon.svelte'
  import ProblemText from '../components/ProblemText.svelte'
  import Switch from '../components/Switch.svelte'
  import { Problem, toProblem } from '../lib/errors'
  import { checkForUpdates, hostStatus, installUpdate, setAutoUpdate, setHosting } from '../lib/platform'
  import type { ControlStatus } from '../lib/types'

  let { onback }: { onback: () => void } = $props()

  let status = $state<ControlStatus | null>(null)
  let loaded = $state(false)
  let busy = $state(false)
  let error = $state<Problem | null>(null)
  let updateBusy = $state(false)
  let updateError = $state<Problem | null>(null)
  let autoUpdateError = $state<Problem | null>(null)
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

  async function runUpdate(action: () => Promise<ControlStatus>, failure: string) {
    updateBusy = true
    updateError = null
    try {
      status = await action()
    } catch (e) {
      updateError = toProblem(e, failure)
    }
    updateBusy = false
  }

  async function toggleAutoUpdate(enabled: boolean) {
    updateBusy = true
    autoUpdateError = null
    try {
      status = await setAutoUpdate(enabled)
    } catch (e) {
      autoUpdateError = toProblem(
        e,
        enabled ? 'Couldn’t turn on automatic updates.' : 'Couldn’t turn off automatic updates.',
      )
    }
    updateBusy = false
  }

  const update = $derived(status?.update)
  const updateProblem = $derived(
    update?.state === 'failed'
      ? new Problem(
          update.installing
            ? status?.autoUpdate
              ? `This PC is still on ${status.version}. It tries again in a few hours.`
              : `This PC is still on ${status?.version}. Check again to retry.`
            : status?.autoUpdate
              ? 'It tries again in a few hours.'
              : 'Check this PC’s internet connection, then check again.',
          update.message,
        )
      : null,
  )

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
          <span class="note" id="hosting-note">{hostingNote}</span>
          {#if error}<div class="note error" role="alert"><ProblemText problem={error} /></div>{/if}
        </div>
        <Switch
          checked={status?.hosting ?? false}
          disabled={busy || !status}
          labelledby="hosting-label"
          describedby="hosting-note"
          onchange={toggle}
        />
      </div>
    </div>
  </section>

  {#if status && update}
    <section aria-labelledby="updates">
      <h2 id="updates">Updates</h2>
      <div class="group">
        <div class="row">
          <div class="text">
            <div class="text" id="update-status" aria-live="polite">
              {#if update.state === 'checking'}
                <span class="label">Checking for updates…</span>
              {:else if update.state === 'upToDate'}
                <span class="label">Conductor is up to date</span>
              {:else if update.state === 'ready'}
                <span class="label">Conductor {update.version} is ready to install</span>
                <span class="note">Installing restarts Conductor, which takes about a minute. Anyone connected to this PC is disconnected until it’s back.</span>
              {:else if update.state === 'installing'}
                <span class="label">Installing Conductor {update.version}…</span>
                <span class="note">This app closes and opens again when it’s done.</span>
              {:else if updateProblem}
                <span class="label">Couldn’t update Conductor</span>
                <div class="note error"><ProblemText problem={updateProblem} /></div>
              {:else}
                <span class="label">Updates haven’t been checked yet</span>
              {/if}
            </div>
            {#if updateError}<div class="note error" role="alert"><ProblemText problem={updateError} /></div>{/if}
          </div>
          {#if update.state === 'ready'}
            <button
              class="btn primary"
              disabled={updateBusy}
              aria-describedby="update-status"
              onclick={() => runUpdate(installUpdate, 'Couldn’t start the update.')}>Install now</button
            >
          {:else if update.state !== 'installing' && update.state !== 'checking'}
            <button
              class="btn"
              disabled={updateBusy}
              aria-describedby="update-status"
              onclick={() => runUpdate(checkForUpdates, 'Couldn’t check for updates.')}
              >{update.state === 'failed' ? 'Check again' : 'Check for updates'}</button
            >
          {/if}
        </div>
        <div class="row">
          <div class="text">
            <span class="label" id="auto-update-label">Install updates automatically</span>
            <span class="note" id="auto-update-note">Updates install when this app is closed and nobody is connected to this PC.</span>
            {#if autoUpdateError}<div class="note error" role="alert"><ProblemText problem={autoUpdateError} /></div>{/if}
          </div>
          <Switch
            checked={status.autoUpdate}
            disabled={updateBusy}
            labelledby="auto-update-label"
            describedby="auto-update-note"
            onchange={toggleAutoUpdate}
          />
        </div>
      </div>
    </section>
  {/if}

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

  .row .btn {
    flex: none;
  }

  @media (max-width: 480px) {
    main {
      padding-inline: var(--space-4);
    }
  }
</style>
