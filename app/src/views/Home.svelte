<script lang="ts">
  import { tick } from 'svelte'
  import Icon from '../components/Icon.svelte'
  import ProblemText from '../components/ProblemText.svelte'
  import Schedule from '../components/Schedule.svelte'
  import { confirmWake, deviceUrl, power, reachable, Unreachable, wake } from '../lib/api'
  import { Problem, toProblem } from '../lib/errors'
  import { devicesFrom, isDesktopApp, isPhoneApp, knownPcs, listDevices } from '../lib/platform'
  import { summary } from '../lib/schedule'
  import type { Device, HostInfo, PowerAction, PowerSchedule, WakeAdapter } from '../lib/types'
  import { waitUntil, wakerFor } from '../lib/wake'

  let {
    onconnect,
    onsettings,
  }: { onconnect: (t: { url: string; info: HostInfo }) => void; onsettings: () => void } = $props()

  /** Every device, including the one this runs on, which can wake the others. */
  let all = $state<Device[] | null>(null)
  const list = $derived(all?.filter((d) => !d.isYou) ?? null)
  let error = $state<Problem | null>(null)

  type Action = PowerAction | 'testWake'
  /** What's going on with a device beyond what the list says. */
  type Row =
    | { kind: 'confirm'; action: Action }
    | { kind: 'busy'; text: string; waking: boolean }
    /** Put to sleep or shut down from here, until it's back. */
    | { kind: 'down'; text: string; asleep: boolean; at: number }
    | { kind: 'note'; text: string; problem: boolean }
    | { kind: 'schedule' }
  /** By device ID. Replaced on every change, so a timer can tell whether its row is still there. */
  let rows = $state.raw<Record<string, Row>>({})
  let menuFor = $state<string | null>(null)

  /** Schedules just changed from here, by device ID, until the list has caught up. */
  let changedSchedules = $state.raw<Record<string, { schedule: PowerSchedule[]; at: number }>>({})

  /** Pictures of PCs' screens, by device ID, as object URLs. */
  let previews = $state.raw<Record<string, string>>({})
  /** When each picture was last asked for. */
  const previewAt = new Map<string, number>()
  const PREVIEW_EVERY = 30_000

  /** The phone app asks one of the owner's PCs for the list; this is for naming one. */
  let askPc = $state(isPhoneApp && knownPcs().length === 0)
  let pcName = $state('')
  let finding = $state(false)
  let pcProblem = $state<Problem | null>(null)

  async function findDevices(e: SubmitEvent) {
    e.preventDefault()
    const name = pcName.trim()
    if (!name || finding) return
    finding = true
    try {
      all = await devicesFrom(name)
      error = null
      pcProblem = null
      askPc = false
    } catch (e) {
      pcProblem =
        e instanceof Unreachable
          ? new Problem(`Couldn’t reach ${name}. Check the name, and that the PC is on and runs Conductor.`, e.details)
          : toProblem(e, `Couldn’t get the list of devices from ${name}.`)
      // Back to the name, in case the button that was pressed lost focus.
      document.getElementById('pc-name')?.focus()
    } finally {
      finding = false
    }
  }

  async function usePc() {
    askPc = true
    await tick()
    document.getElementById('pc-name')?.focus()
  }

  async function load() {
    if (askPc) return
    try {
      all = await listDevices()
      error = null
      refreshPreviews(all)
      const fresh = Object.entries(changedSchedules).filter(([, c]) => Date.now() - c.at < 15_000)
      if (fresh.length < Object.keys(changedSchedules).length) changedSchedules = Object.fromEntries(fresh)
      // A PC put to sleep or shut down from here answers again. The list can
      // lag a few seconds behind, so only trust it once that's passed.
      for (const d of all) {
        const row = rows[d.id]
        if (d.host && row?.kind === 'down' && Date.now() - row.at > 15_000) set(d.id, null)
      }
    } catch (e) {
      // Keep showing the last good list; only an empty screen needs the error.
      if (!all) error = toProblem(e, 'Couldn’t get the list of devices.')
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

  // Tracks nothing, so this only runs when the list goes away.
  let gone = false
  $effect(() => () => {
    gone = true
    for (const url of Object.values(previews)) URL.revokeObjectURL(url)
  })

  /** Asks PCs that share a picture of their screen for a new one now and then. */
  function refreshPreviews(list: Device[]) {
    const now = Date.now()
    const ids = new Set(list.map((d) => d.id))
    for (const id of Object.keys(previews)) if (!ids.has(id)) showPreview(id, null)
    for (const id of previewAt.keys()) if (!ids.has(id)) previewAt.delete(id)
    for (const d of list) {
      if (d.isYou) continue
      if (!d.host?.preview || rows[d.id]?.kind === 'down') {
        previewAt.delete(d.id)
        if (previews[d.id]) showPreview(d.id, null)
        continue
      }
      if (now - (previewAt.get(d.id) ?? 0) < PREVIEW_EVERY) continue
      previewAt.set(d.id, now)
      fetchPreview(d)
    }
  }

  async function fetchPreview(d: Device) {
    try {
      const res = await fetch(`${deviceUrl(d)}/api/preview`, { signal: AbortSignal.timeout(8000) })
      const picture = res.ok ? await res.blob() : null
      // Asked for again if it's still listed next time.
      if (gone || !previewAt.has(d.id)) return
      showPreview(d.id, picture && URL.createObjectURL(picture))
    } catch {
      // The last picture stays until the PC answers again or leaves the list.
    }
  }

  function showPreview(id: string, url: string | null) {
    const next = { ...previews }
    if (next[id]) URL.revokeObjectURL(next[id])
    if (url) next[id] = url
    else delete next[id]
    previews = next
  }

  function retry() {
    error = null
    all = null
    load()
  }

  function set(id: string, row: Row | null) {
    const next = { ...rows }
    if (row) next[id] = row
    else delete next[id]
    rows = next
  }

  /** A passing note on a device's row. Problems stay up longer. */
  function note(id: string, text: string, problem = false) {
    const row: Row = { kind: 'note', text, problem }
    set(id, row)
    setTimeout(() => rows[id] === row && set(id, null), problem ? 60_000 : 15_000)
  }

  const offline = (url: string) => async () => !(await reachable(url))

  async function runPower(d: Device, action: PowerAction) {
    const url = deviceUrl(d)
    const doing = { sleep: 'Going to sleep…', restart: 'Restarting…', shutDown: 'Shutting down…' }[action]
    const verb = { sleep: 'go to sleep', restart: 'restart', shutDown: 'shut down' }[action]
    set(d.id, { kind: 'busy', text: doing, waking: false })
    try {
      await power(url, action)
    } catch {
      note(d.id, `Couldn’t ask ${d.name} to ${verb}. Try again.`, true)
      return
    }
    if (!(await waitUntil(offline(url), 90_000))) {
      note(d.id, `${d.name} didn’t ${verb}. Connect to see what’s holding it up.`, true)
      return
    }
    if (action === 'restart') {
      if (await waitUntil(() => reachable(url), 5 * 60_000, 3000)) {
        set(d.id, null)
        load()
      } else {
        note(d.id, `${d.name} hasn’t come back since restarting.`, true)
      }
      return
    }
    const asleep = action === 'sleep'
    set(d.id, { kind: 'down', text: asleep ? 'Asleep' : 'Turned off', asleep, at: Date.now() })
  }

  /** Sends wake packets every 20 seconds until the PC answers, for up to 2 minutes. */
  async function wakeAndWait(
    d: Device,
    adapters: WakeAdapter[],
    waker: Device,
  ): Promise<'woke' | 'asleep' | 'failed'> {
    const url = deviceUrl(d)
    const deadline = Date.now() + 120_000
    while (Date.now() < deadline) {
      try {
        await wake(deviceUrl(waker), adapters)
      } catch {
        return 'failed'
      }
      if (await waitUntil(() => reachable(url), Math.min(20_000, deadline - Date.now()), 3000)) return 'woke'
    }
    return 'asleep'
  }

  function wakerName(waker: Device) {
    return waker.isYou ? 'This PC' : waker.name
  }

  async function runWake(d: Device) {
    const waker = wakerFor(d, all ?? [])
    if (!waker || !d.wake) return
    set(d.id, { kind: 'busy', text: 'Waking…', waking: true })
    switch (await wakeAndWait(d, d.wake.adapters, waker)) {
      case 'woke':
        set(d.id, null)
        load()
        break
      case 'asleep':
        note(d.id, `${d.name} didn’t wake. Try again, or wake it at the PC.`, true)
        break
      case 'failed':
        note(d.id, `${wakerName(waker)} couldn’t send the wake signal. Try again.`, true)
    }
  }

  async function runTestWake(d: Device) {
    const waker = wakerFor(d, all ?? [])
    const adapters = d.wake?.adapters
    if (!waker || !adapters) return
    const url = deviceUrl(d)
    set(d.id, { kind: 'busy', text: 'Going to sleep…', waking: false })
    try {
      await power(url, 'sleep')
    } catch {
      note(d.id, `Couldn’t ask ${d.name} to go to sleep.`, true)
      return
    }
    if (!(await waitUntil(offline(url), 90_000))) {
      note(d.id, `${d.name} didn’t go to sleep, so waking wasn’t tested.`, true)
      return
    }
    // A PC that has only just dropped off the network may not be listening for wake packets yet.
    await new Promise((r) => setTimeout(r, 15_000))
    set(d.id, { kind: 'busy', text: 'Waking…', waking: true })
    const result = await wakeAndWait(d, adapters, waker)
    if (result !== 'woke') {
      const why =
        result === 'failed'
          ? `${wakerName(waker)} couldn’t send the wake signal.`
          : 'Its network adapter may not be set up to wake it.'
      note(d.id, `${d.name} didn’t wake. ${why} Press a key or the power button on ${d.name} to wake it.`, true)
      return
    }
    try {
      await confirmWake(url)
    } catch {
      note(d.id, `${d.name} woke up, but didn’t save that waking works. Try the test again.`, true)
      return
    }
    note(d.id, `Waking works. You can wake ${d.name} from here while it’s asleep.`)
    load()
  }

  function canTestWake(d: Device) {
    return !!d.wake && !d.wake.verified && wakerFor(d, all ?? []) !== null
  }

  /** The PC that would wake `d`, when waking it has been tested and it isn't on. */
  function wakeable(d: Device, row: Row | undefined) {
    if (!d.wake?.verified || (d.host && row?.kind !== 'down')) return null
    return wakerFor(d, all ?? [])
  }

  async function ask(d: Device, action: Action) {
    menuFor = null
    set(d.id, { kind: 'confirm', action })
    await tick()
    const safe = action === 'restart' || action === 'shutDown' ? '.btn:not(.primary)' : '.btn.primary'
    document.querySelector<HTMLElement>(`[data-confirm="${d.id}"] ${safe}`)?.focus()
  }

  /** Starts an action and keeps keyboard focus on its row while the buttons are gone. */
  async function start(d: Device, run: () => Promise<void>) {
    run()
    await tick()
    document.querySelector<HTMLElement>(`[data-row="${d.id}"]`)?.focus()
  }

  async function cancel(d: Device) {
    set(d.id, null)
    await tick()
    document.querySelector<HTMLElement>(`[data-more="${d.id}"]`)?.focus()
  }

  function go(d: Device, action: Action) {
    start(d, () => (action === 'testWake' ? runTestWake(d) : runPower(d, action)))
  }

  function question(d: Device, action: Action) {
    const waker = wakerFor(d, all ?? [])
    switch (action) {
      case 'sleep':
        return d.wake?.verified && waker
          ? `Put ${d.name} to sleep? You can wake it from here.`
          : `Put ${d.name} to sleep? Someone will need to wake it at the PC.`
      case 'restart':
        return `Restart ${d.name}? Apps with unsaved work can hold it up.`
      case 'shutDown':
        return `Shut down ${d.name}? Someone will need to turn it back on at the PC.`
      case 'testWake': {
        const by = waker?.isYou ? 'this PC' : waker?.name
        return `Test waking ${d.name}? It goes to sleep, then ${by} wakes it within two minutes. If that fails, someone will need to wake it at the PC.`
      }
    }
  }

  const labels: Record<Action, string> = {
    sleep: 'Sleep',
    restart: 'Restart',
    shutDown: 'Shut down',
    testWake: 'Test waking',
  }

  async function toggleMenu(d: Device) {
    menuFor = menuFor === d.id ? null : d.id
    await tick()
    document.querySelector<HTMLElement>(`[data-menu="${d.id}"] [role="menuitem"]`)?.focus()
  }

  function onMenuKey(e: KeyboardEvent, d: Device) {
    const items = Array.from((e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>('[role="menuitem"]'))
    const at = items.indexOf(document.activeElement as HTMLElement)
    let next: number
    switch (e.key) {
      case 'ArrowDown':
        next = (at + 1) % items.length
        break
      case 'ArrowUp':
        next = (at - 1 + items.length) % items.length
        break
      case 'Home':
        next = 0
        break
      case 'End':
        next = items.length - 1
        break
      case 'Escape':
        e.preventDefault()
        menuFor = null
        document.querySelector<HTMLElement>(`[data-more="${d.id}"]`)?.focus()
        return
      case 'Tab':
        menuFor = null
        return
      default:
        return
    }
    e.preventDefault()
    items[next]?.focus()
  }

  function closeMenuOutside(e: PointerEvent) {
    if (menuFor && !(e.target as Element).closest?.('.more')) menuFor = null
  }

  async function openSchedule(d: Device) {
    menuFor = null
    set(d.id, { kind: 'schedule' })
    await tick()
    document.querySelector<HTMLElement>(`[data-row="${d.id}"] select`)?.focus()
  }

  function scheduleOf(d: Device) {
    return changedSchedules[d.id]?.schedule ?? d.host?.schedule
  }

  function availability(d: Device): 'ready' | 'not-running' | 'offline' {
    if (d.host) return 'ready'
    return d.online && !d.silent ? 'not-running' : 'offline'
  }

  function describe(d: Device, row: Row | undefined) {
    if (row && row.kind !== 'confirm' && row.kind !== 'schedule') return row.text
    switch (availability(d)) {
      case 'ready': {
        const n = d.host!.displays.length
        const displays = `${n} ${n === 1 ? 'display' : 'displays'}`
        const scheduled = summary(scheduleOf(d) ?? [])
        return scheduled ? `${displays}, ${scheduled}` : displays
      }
      case 'not-running':
        return 'Conductor isn’t running'
      case 'offline': {
        // Tailscale can call a PC online for hours after it went to sleep.
        const seen = d.silent
          ? 'Not answering, likely asleep'
          : d.lastSeen
            ? `Offline, last seen ${ago(d.lastSeen)}`
            : 'Offline'
        return d.wake?.verified && !wakerFor(d, all ?? []) ? `${seen}. To wake it, turn on another PC on its network.` : seen
      }
    }
  }

  /** Classes for the row, which set the status dot. */
  function rowClass(d: Device, row: Row | undefined) {
    if (row?.kind === 'busy') return row.waking ? 'asleep busy' : `${availability(d)} busy`
    if (row?.kind === 'down') return row.asleep ? 'asleep' : 'offline'
    return availability(d)
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

<svelte:window onpointerdown={closeMenuOutside} />

<main>
  <header>
    <h1>Devices</h1>
    {#if isDesktopApp}
      <button class="link" onclick={onsettings}>Settings</button>
    {/if}
  </header>

  {#if askPc}
    <form class="empty" onsubmit={findDevices}>
      <label for="pc-name">Enter the name of one of your PCs that runs Conductor, as the Tailscale app shows it.</label>
      <div class="find">
        <input
          id="pc-name"
          bind:value={pcName}
          required
          autocapitalize="off"
          autocomplete="off"
          spellcheck="false"
          enterkeyhint="go"
          aria-describedby={pcProblem ? 'pc-problem' : undefined}
        />
        <button class="btn primary" disabled={finding}>{finding ? 'Finding…' : 'Find devices'}</button>
      </div>
      {#if pcProblem}
        <div class="problem" id="pc-problem" role="alert">
          <ProblemText problem={pcProblem} />
        </div>
      {/if}
    </form>
  {:else if error}
    <div class="problem" role="alert">
      <ProblemText problem={error} />
      <div class="buttons">
        <button class="btn" onclick={retry}>Try again</button>
        {#if isPhoneApp}
          <button class="link" onclick={usePc}>Use another PC</button>
        {/if}
      </div>
    </div>
  {:else if list === null}
    <p class="quiet">Looking for your devices…</p>
  {:else if list.length === 0}
    <div class="empty">
      <p class="quiet">Install Conductor on a Windows PC signed in to this Tailscale account to see it here.</p>
      <button class="btn" onclick={retry}>Check again</button>
    </div>
  {:else}
    <ul class="devices">
      {#each list as d (d.id)}
        {@const row = rows[d.id]}
        {@const waker = wakeable(d, row)}
        {@const preview = availability(d) === 'ready' && row?.kind !== 'down' ? previews[d.id] : undefined}
        <li class={rowClass(d, row)} data-row={d.id} tabindex="-1">
          <!-- What's on its main display, or blank like a screen that's off. -->
          <div class="screen" aria-hidden="true">
            {#if preview}<img src={preview} alt="" />{/if}
          </div>
          <div class="device">
            <span class="name">{d.name}</span>
            <span class="meta" class:problem={row?.kind === 'note' && row.problem} aria-live="polite">
              <span class="status" aria-hidden="true"></span>
              <span>{describe(d, row)}</span>
            </span>
          </div>
          <div class="actions">
            {#if row?.kind === 'busy' || row?.kind === 'confirm' || row?.kind === 'schedule'}
              <!-- The row says what's going on. -->
            {:else if waker}
              <button class="btn primary" aria-label="Wake {d.name}" onclick={() => start(d, () => runWake(d))}>Wake</button>
            {:else if d.host && row?.kind !== 'down'}
              <button
                class="btn primary"
                aria-label="Connect to {d.name}"
                onclick={() => onconnect({ url: deviceUrl(d), info: d.host! })}
              >
                Connect
              </button>
              {#if d.host.wake}
                <div class="more">
                  <button
                    class="btn icon"
                    data-more={d.id}
                    aria-haspopup="menu"
                    aria-expanded={menuFor === d.id}
                    aria-label="More for {d.name}"
                    onclick={() => toggleMenu(d)}
                  >
                    <Icon name="more" />
                  </button>
                  {#if menuFor === d.id}
                    <div
                      class="menu"
                      role="menu"
                      tabindex="-1"
                      aria-label="More for {d.name}"
                      data-menu={d.id}
                      onkeydown={(e) => onMenuKey(e, d)}
                    >
                      <button role="menuitem" tabindex="-1" onclick={() => ask(d, 'sleep')}>Sleep</button>
                      <button role="menuitem" tabindex="-1" onclick={() => ask(d, 'restart')}>Restart</button>
                      <button role="menuitem" tabindex="-1" onclick={() => ask(d, 'shutDown')}>Shut down</button>
                      {#if d.host.schedule}
                        <button role="menuitem" tabindex="-1" onclick={() => openSchedule(d)}>Schedule</button>
                      {/if}
                      {#if canTestWake(d)}
                        <button role="menuitem" tabindex="-1" onclick={() => ask(d, 'testWake')}>Test waking</button>
                      {/if}
                    </div>
                  {/if}
                </div>
              {/if}
            {/if}
          </div>
          {#if row?.kind === 'confirm'}
            {@const action = row.action}
            {@const onkeydown = (e: KeyboardEvent) => e.key === 'Escape' && cancel(d)}
            <div class="confirm" role="group" aria-labelledby="ask-{d.id}" data-confirm={d.id}>
              <p id="ask-{d.id}">{question(d, action)}</p>
              <div class="buttons">
                <button class="btn primary" {onkeydown} onclick={() => go(d, action)}>{labels[action]}</button>
                <button class="btn" {onkeydown} onclick={() => cancel(d)}>Cancel</button>
              </div>
            </div>
          {/if}
          {#if row?.kind === 'schedule' && d.host}
            <Schedule
              id={d.id}
              host={deviceUrl(d)}
              name={d.name}
              schedule={scheduleOf(d) ?? []}
              onchange={(schedule) => (changedSchedules = { ...changedSchedules, [d.id]: { schedule, at: Date.now() } })}
              onclose={() => cancel(d)}
            />
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
    column-gap: var(--space-4);
    row-gap: var(--space-3);
    min-height: 64px;
    padding: var(--space-3) 0;
    border-bottom: 1px solid var(--border);
  }

  .screen {
    width: 80px;
    aspect-ratio: 16 / 10;
    overflow: hidden;
    border-radius: 4px;
    background: var(--border);
  }

  .screen img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .status {
    flex: none;
    width: 8px;
    height: 8px;
    /* Centered on the first line of the text after it. */
    margin-top: 5px;
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

  .asleep .status {
    background: var(--status-asleep);
  }

  .busy .status {
    animation: working 1.2s ease-in-out infinite alternate;
  }

  @keyframes working {
    to {
      opacity: 0.3;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .busy .status {
      animation: none;
    }
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
  .asleep .name,
  .not-running .name {
    color: var(--text-muted);
  }

  .meta {
    display: flex;
    gap: 6px;
    font-size: var(--text-sm);
    line-height: 18px;
    color: var(--text-muted);
  }

  .meta.problem {
    color: var(--status-error);
  }

  .actions {
    display: flex;
    gap: var(--space-2);
  }

  .btn.icon {
    width: 36px;
    padding: 0;
  }

  .more {
    position: relative;
  }

  .menu {
    position: absolute;
    top: calc(100% + var(--space-1));
    right: 0;
    z-index: 1;
    display: flex;
    flex-direction: column;
    min-width: 168px;
    padding: var(--space-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  .menu button {
    min-height: 36px;
    padding: 0 var(--space-3);
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    text-align: left;
    cursor: pointer;
  }

  .menu button:hover,
  .menu button:focus-visible {
    background: var(--surface-hover);
  }

  .menu button:focus-visible {
    outline-offset: -2px;
  }

  .confirm {
    grid-column: 2 / -1;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
  }

  .confirm p {
    margin: 0;
    max-width: 60ch;
  }

  .buttons {
    display: flex;
    gap: var(--space-2);
  }

  .problem,
  .empty {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-3);
    max-width: 52ch;
  }

  .problem p,
  .empty p {
    margin: 0;
  }

  .problem .buttons {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }

  .find {
    display: flex;
    gap: var(--space-2);
    width: 100%;
  }

  input {
    flex: 1;
    min-width: 0;
    min-height: 36px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface);
  }

  @media (pointer: coarse) {
    input {
      min-height: var(--touch-target);
    }
  }

  @media (pointer: coarse) {
    .btn.icon {
      width: var(--touch-target);
    }

    .menu button {
      min-height: var(--touch-target);
    }
  }

  @media (max-width: 480px) {
    main {
      padding-inline: var(--space-4);
    }

    li {
      column-gap: var(--space-3);
    }

    .screen {
      width: 64px;
    }
  }
</style>
