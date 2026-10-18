<script lang="ts">
  import { tick } from 'svelte'
  import Icon from '../components/Icon.svelte'
  import ProblemText from '../components/ProblemText.svelte'
  import Schedule from '../components/Schedule.svelte'
  import { confirmWake, deviceUrl, power, reachable, Unreachable, wake } from '../lib/api'
  import { Problem, toProblem } from '../lib/errors'
  import {
    devicesFrom,
    devicesFromNearby,
    installPhoneApp,
    isDesktopApp,
    isPhoneApp,
    knownPcs,
    listDevices,
    nearbyPcs,
    newerPhoneApp,
  } from '../lib/platform'
  import { summary } from '../lib/schedule'
  import { share } from '../lib/share.svelte'
  import type { Device, DisplayInfo, HostInfo, NearbyPc, PowerAction, PowerSchedule, WakeAdapter } from '../lib/types'
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
  let heading: HTMLElement

  /** Schedules just changed from here, by device ID, until the list has caught up. */
  let changedSchedules = $state.raw<Record<string, { schedule: PowerSchedule[]; at: number }>>({})

  /** A newer phone app, and how updating to it is going. */
  let phoneUpdate = $state<{ version: string; state: 'out' | 'downloading' | 'asked'; problem: Problem | null } | null>(
    null,
  )
  if (isPhoneApp) newerPhoneApp().then((version) => version && (phoneUpdate = { version, state: 'out', problem: null }))

  async function updatePhoneApp() {
    if (!phoneUpdate) return
    const { version } = phoneUpdate
    phoneUpdate = { version, state: 'downloading', problem: null }
    try {
      await installPhoneApp(version)
      phoneUpdate = { version, state: 'asked', problem: null }
    } catch (e) {
      phoneUpdate = { version, state: 'out', problem: toProblem(e, `Couldn’t download Conductor ${version}.`) }
    }
  }

  /** Pictures of PCs' screens, by device ID, as object URLs. */
  let previews = $state.raw<Record<string, string>>({})
  /** When each picture was last asked for. */
  const previewAt = new Map<string, number>()
  const PREVIEW_EVERY = 30_000

  /**
   * The phone app asks one of the owner's PCs for the list. To find one, it
   * looks on the phone's Wi-Fi, and when none answers there, the owner names one.
   */
  const noPcs = isPhoneApp && knownPcs().length === 0
  let askPc = $state(noPcs)
  let pcName = $state('')
  let finding = $state(false)
  let pcProblem = $state<Problem | null>(null)
  let looking = $state(false)
  /** Looked on the Wi-Fi since the owner was last asked for a PC. */
  let looked = $state(false)
  let noneNearby = $state(false)
  /** PCs answered on the Wi-Fi, but didn't give the device list. */
  let nearbyProblem = $state<Problem | null>(null)

  /** Looks for PCs on the phone's Wi-Fi, and lists the devices from the first that answers. */
  async function lookNearby() {
    looking = true
    let found: NearbyPc[] = []
    let problem = null
    try {
      found = await nearbyPcs()
      // Unless a PC named meanwhile answered first.
      const list = found.length && askPc ? await devicesFromNearby(found) : null
      if (list && askPc) {
        all = list
        error = null
        askPc = false
      }
    } catch (e) {
      problem = toProblem(e, 'Couldn’t get the list of devices.')
    }
    // Both at once, so what's shown about the last look stays until there's news.
    noneNearby = !problem && found.length === 0
    nearbyProblem = problem
    looking = false
    looked = true
  }

  if (noPcs) lookNearby()

  async function lookAgain() {
    if (looking) return
    await lookNearby()
    if (askPc) return
    // The button pressed went away with the form.
    await tick()
    heading?.focus()
  }

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
    looked = true
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
    // The button goes away while it looks.
    heading.focus()
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

  /**
   * Where a PC's displays sit relative to each other, as a drawing for its
   * screen when there's no picture of it.
   */
  function arrangement(displays: DisplayInfo[]) {
    if (!displays.length) return null
    const left = Math.min(...displays.map((d) => d.x))
    const top = Math.min(...displays.map((d) => d.y))
    const width = Math.max(...displays.map((d) => d.x + d.width)) - left
    const height = Math.max(...displays.map((d) => d.y + d.height)) - top
    // Displays that touch are drawn apart.
    const gap = Math.max(width, height) * 0.015
    const rects = displays.map((d) => ({
      x: d.x - left + gap,
      y: d.y - top + gap,
      width: Math.max(d.width - 2 * gap, gap),
      height: Math.max(d.height - 2 * gap, gap),
      primary: d.primary,
    }))
    return { width, height, rects }
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
        // Onward from the power button, as if the menu had never opened.
        document.querySelector<HTMLElement>(`[data-more="${d.id}"]`)?.focus()
        menuFor = null
        return
      default:
        return
    }
    e.preventDefault()
    items[next]?.focus()
  }

  function closeMenuOutside(e: PointerEvent) {
    if (!menuFor || (e.target as Element).closest?.('.more')) return
    menuFor = null
    // Like any menu, the click that closes it does nothing else, such as connecting.
    const swallow = (c: MouseEvent) => {
      c.stopPropagation()
      c.preventDefault()
    }
    window.addEventListener('click', swallow, { capture: true, once: true })
    // A press that never became a click.
    const forget = () => window.removeEventListener('click', swallow, { capture: true })
    window.addEventListener('pointerdown', forget, { capture: true, once: true })
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

  /** Probably asleep rather than off. */
  function asleep(d: Device, row: Row | undefined) {
    if (row?.kind === 'down') return row.asleep
    return !!d.silent || !!d.wake?.verified
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

  // Files shared to the phone app from other apps go to the PC connected to next.
  function sharedText() {
    const { files, text, unreadable } = share
    const notRead = unreadable === 1 ? 'One file couldn’t be read.' : `${unreadable} files couldn’t be read.`
    if (files.length) {
      const what = files.length === 1 ? files[0].name : `${files.length} files`
      return `Connect to a PC to send ${what} to its Downloads folder.${unreadable ? ` ${notRead}` : ''}`
    }
    if (text) return 'Only files can be sent to a PC, not text.'
    return `${notRead} Try sharing again, or from another app.`
  }
</script>

<svelte:window onpointerdown={closeMenuOutside} />

<main>
  <header>
    <h1 bind:this={heading} tabindex="-1">Your PCs</h1>
    {#if isDesktopApp}
      <button class="btn quiet" onclick={onsettings}><Icon name="settings" />Settings</button>
    {/if}
  </header>

  <!-- Always there, so screen readers announce what appears in it. -->
  <div role="status">
    {#if share.any}
      <div class="shared">
        <span class="shared-icon"><Icon name="file" /></span>
        <p>{sharedText()}</p>
        <button class="btn quiet" onclick={() => share.dismiss()}>{share.files.length ? 'Don’t send' : 'OK'}</button>
      </div>
    {/if}
    {#if phoneUpdate}
      <div class="shared update">
        <span class="shared-icon"><Icon name="update" /></span>
        <div class="update-text">
          <p>
            {#if phoneUpdate.state === 'downloading'}
              Downloading Conductor {phoneUpdate.version}…
            {:else if phoneUpdate.state === 'asked'}
              Android will ask to install Conductor {phoneUpdate.version}. If it doesn’t, allow Conductor to install
              apps, then try again.
            {:else}
              Conductor {phoneUpdate.version} is out.
            {/if}
          </p>
          {#if phoneUpdate.problem}<ProblemText problem={phoneUpdate.problem} />{/if}
        </div>
        <div class="update-actions">
          {#if phoneUpdate.state !== 'downloading'}
            <button class="btn" onclick={updatePhoneApp}>{phoneUpdate.state === 'asked' ? 'Try again' : 'Update'}</button>
          {/if}
          <button class="btn quiet" onclick={() => (phoneUpdate = null)}>Not now</button>
        </div>
      </div>
    {/if}
    {#if askPc && looking}
      <p class="quiet nearby-status">Looking for PCs on this Wi-Fi…</p>
    {:else if askPc && noneNearby}
      <p class="nearby-status">No PCs running Conductor answered on this Wi-Fi.</p>
    {/if}
  </div>

  {#if askPc && looked}
    <form class="find-pc" onsubmit={findDevices}>
      {#if noneNearby || nearbyProblem}
        <div class="nearby">
          {#if nearbyProblem}
            {#key nearbyProblem}
              <div class="problem" role="alert"><ProblemText problem={nearbyProblem} /></div>
            {/key}
          {/if}
          <button type="button" class="btn" aria-disabled={looking} onclick={lookAgain}>
            {nearbyProblem ? 'Try again' : 'Look again'}
          </button>
        </div>
      {/if}
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
        <button class="btn primary" disabled={finding}>{finding ? 'Finding…' : 'Find PCs'}</button>
      </div>
      {#if pcProblem}
        <div class="problem" id="pc-problem" role="alert">
          <ProblemText problem={pcProblem} />
        </div>
      {/if}
    </form>
  {:else if askPc}
    <!-- Looking on the Wi-Fi, which the status above says. -->
  {:else if error}
    <div class="problem" role="alert">
      <ProblemText problem={error} />
      <div class="buttons">
        <button class="btn" onclick={retry}>Try again</button>
        {#if isPhoneApp}
          <button class="btn quiet" onclick={usePc}>Use another PC</button>
        {/if}
      </div>
    </div>
  {:else if list === null}
    <p class="quiet">Looking for your PCs…</p>
  {:else if list.length === 0}
    <div class="empty">
      <p>Install Conductor on a Windows PC signed in to this Tailscale account to see it here.</p>
      <button class="btn" onclick={retry}>Check again</button>
    </div>
  {:else}
    <ul class="devices">
      {#each list as d (d.id)}
        {@const row = rows[d.id]}
        {@const waker = wakeable(d, row)}
        {@const on = availability(d) === 'ready' && row?.kind !== 'down'}
        {@const preview = on ? previews[d.id] : undefined}
        {@const osd = row?.kind === 'busy' || row?.kind === 'confirm'}
        {@const map = on && !preview && !osd && d.host ? arrangement(d.host.displays) : null}
        {@const connectable = on && !waker && !osd && row?.kind !== 'schedule'}
        {@const powered = connectable && !!d.host?.wake}
        <li class={rowClass(d, row)} data-row={d.id} tabindex="-1">
          <!-- A larger place to click. The keyboard and screen readers use the Connect button below. -->
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <div
            class="screen"
            class:connectable
            onclick={(e) => {
              // Not a button or menu on the screen, which can make it connectable by the time the click gets here.
              if (!connectable || (e.target as Element).closest('button, [role="menu"]')) return
              onconnect({ url: deviceUrl(d), info: d.host! })
            }}
            oncontextmenu={(e) => {
              if (!powered) return
              e.preventDefault()
              toggleMenu(d)
            }}
          >
            {#if preview}
              <div class="picture" class:dim={osd} aria-hidden="true"><img src={preview} alt="" /></div>
            {:else if map}
              <!-- Its screen isn't shared, so this draws how its displays are arranged. -->
              <svg class="map" viewBox="0 0 {map.width} {map.height}" aria-hidden="true">
                {#each map.rects as r, i (i)}
                  <rect
                    x={r.x}
                    y={r.y}
                    width={r.width}
                    height={r.height}
                    class:primary={r.primary}
                    vector-effect="non-scaling-stroke"
                  />
                {/each}
              </svg>
            {:else if !osd && (row?.kind === 'down' || availability(d) === 'offline')}
              <!-- Like the light on a monitor that's asleep or off. -->
              <span class="standby" aria-hidden="true"><Icon name={asleep(d, row) ? 'sleep' : 'power'} /></span>
            {/if}
            {#if powered && d.host}
              <div class="more">
                <button
                  class="power"
                  data-more={d.id}
                  aria-haspopup="menu"
                  aria-expanded={menuFor === d.id}
                  aria-label="Power options for {d.name}"
                  onclick={() => toggleMenu(d)}
                >
                  <Icon name="power" />
                </button>
                {#if menuFor === d.id}
                  <div
                    class="menu"
                    role="menu"
                    tabindex="-1"
                    aria-label="Power options for {d.name}"
                    data-menu={d.id}
                    onkeydown={(e) => onMenuKey(e, d)}
                  >
                    <button role="menuitem" tabindex="-1" onclick={() => ask(d, 'sleep')}>
                      <Icon name="sleep" />Sleep
                    </button>
                    <button role="menuitem" tabindex="-1" onclick={() => ask(d, 'restart')}>
                      <Icon name="restart" />Restart
                    </button>
                    <button role="menuitem" tabindex="-1" onclick={() => ask(d, 'shutDown')}>
                      <Icon name="power" />Shut down
                    </button>
                    {#if d.host.schedule || canTestWake(d)}
                      <div class="separator" role="separator"></div>
                    {/if}
                    {#if d.host.schedule}
                      <button role="menuitem" tabindex="-1" onclick={() => openSchedule(d)}>
                        <Icon name="clock" />Schedule
                      </button>
                    {/if}
                    {#if canTestWake(d)}
                      <button role="menuitem" tabindex="-1" onclick={() => ask(d, 'testWake')}>
                        <Icon name="wake" />Test waking
                      </button>
                    {/if}
                  </div>
                {/if}
              </div>
            {/if}
            {#if row?.kind === 'busy'}
              <p class="osd" aria-hidden="true">{row.text}</p>
            {:else if row?.kind === 'confirm'}
              {@const action = row.action}
              {@const onkeydown = (e: KeyboardEvent) => e.key === 'Escape' && cancel(d)}
              <div class="osd confirm" role="group" aria-labelledby="ask-{d.id}" data-confirm={d.id}>
                <p id="ask-{d.id}">{question(d, action)}</p>
                <div class="buttons">
                  <button class="btn primary" {onkeydown} onclick={() => go(d, action)}>{labels[action]}</button>
                  <button class="btn" {onkeydown} onclick={() => cancel(d)}>Cancel</button>
                </div>
              </div>
            {/if}
          </div>
          <div class="plate">
            <div class="device">
              <span class="name">{d.name}</span>
              <span class="meta" class:problem={row?.kind === 'note' && row.problem} aria-live="polite">
                <span class="status" aria-hidden="true"></span>
                <span>{describe(d, row)}</span>
              </span>
            </div>
            <div class="actions">
              {#if row?.kind === 'busy' || row?.kind === 'confirm' || row?.kind === 'schedule'}
                <!-- The screen says what's going on. -->
              {:else if waker}
                <button class="btn" aria-label="Wake {d.name}" onclick={() => start(d, () => runWake(d))}>
                  Wake
                </button>
              {:else if d.host && row?.kind !== 'down'}
                <button
                  class="btn"
                  aria-label="Connect to {d.name}"
                  onclick={() => onconnect({ url: deviceUrl(d), info: d.host! })}
                >
                  Connect
                </button>
              {/if}
            </div>
          </div>
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
    padding: var(--space-8);
    padding-top: max(var(--space-8), env(safe-area-inset-top));
  }

  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    margin-bottom: var(--space-6);
  }

  h1 {
    margin: 0;
    font-family: var(--font-name);
    font-size: var(--text-title);
    font-stretch: 88%;
    font-weight: 650;
    line-height: 1.15;
  }

  h1:focus {
    outline: none;
  }

  header .btn {
    margin-right: calc(-1 * var(--space-3));
    color: var(--text-muted);
  }

  header .btn:hover {
    color: var(--text);
  }

  .shared {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    max-width: 72ch;
    margin-bottom: var(--space-6);
    padding: var(--space-2) var(--space-2) var(--space-2) var(--space-4);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: inset 0 0 0 1px var(--border);
  }

  .shared-icon {
    color: var(--text-muted);
  }

  .shared p {
    flex: 1;
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  /* On a phone, the buttons go under the text. */
  .shared.update {
    flex-wrap: wrap;
  }

  .update-text {
    flex: 1 1 14rem;
    min-width: 0;
  }

  .update-actions {
    display: flex;
    gap: var(--space-2);
    margin-left: auto;
  }

  .shared .btn {
    flex: none;
  }

  .quiet {
    margin: 0;
    color: var(--text-muted);
  }

  .devices {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(300px, 100%), 1fr));
    gap: var(--space-8) var(--space-6);
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    min-width: 0;
    border-radius: var(--radius);
  }

  li:focus-visible {
    outline-offset: var(--space-2);
  }

  /* The PC's screen, dark like a monitor's glass in both themes. */
  .screen {
    position: relative;
    display: grid;
    place-items: center;
    aspect-ratio: 16 / 10;
    border-radius: var(--radius);
    background: var(--glass);
    box-shadow: inset 0 0 0 1px var(--glass-edge);
    color: var(--overlay-text);
  }

  .screen.connectable {
    cursor: pointer;
  }

  .screen.connectable:hover:not(:has(.more:hover)) {
    outline: 2px solid var(--accent);
    outline-offset: 3px;
  }

  .picture {
    position: absolute;
    inset: 0;
    overflow: hidden;
    border-radius: inherit;
  }

  .picture img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
  }

  .picture.dim img {
    filter: brightness(0.3);
  }

  .map {
    position: absolute;
    top: 22%;
    left: 20%;
    width: 60%;
    height: 56%;
    overflow: visible;
  }

  .map rect {
    fill: none;
    stroke: var(--glass-text);
    stroke-opacity: 0.4;
    stroke-width: 1.5px;
  }

  .map rect.primary {
    fill: var(--glass-text);
    fill-opacity: 0.07;
    stroke-opacity: 0.75;
  }

  .standby {
    color: var(--glass-text);
    opacity: 0.35;
  }

  .standby :global(svg) {
    width: 24px;
    height: 24px;
  }

  /* Written on the screen, like a monitor's own messages. */
  .osd {
    position: relative;
    margin: 0;
    padding: var(--space-4) var(--space-6);
    color: var(--overlay-text);
    text-align: center;
  }

  .confirm {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-4);
    color: var(--overlay-text);
  }

  .confirm p {
    margin: 0;
    max-width: 44ch;
  }

  .confirm .buttons {
    justify-content: center;
  }

  .confirm .btn.primary {
    background: var(--overlay-accent);
    color: var(--overlay-accent-text);
  }

  .confirm .btn.primary:hover {
    background: var(--overlay-accent-hover);
  }

  .confirm .btn:not(.primary) {
    border-color: var(--overlay-border-strong);
    background: transparent;
    color: var(--overlay-text);
  }

  .confirm .btn:not(.primary):hover {
    background: var(--overlay-hover);
  }

  .confirm :focus-visible {
    outline-color: var(--overlay-accent);
  }

  /* The label under the screen. */
  .plate {
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    padding-inline: 2px;
  }

  .device {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .name {
    overflow: hidden;
    font-family: var(--font-name);
    font-size: var(--text-name);
    font-stretch: 88%;
    font-weight: 600;
    line-height: 1.25;
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
    font-size: 13px;
    line-height: 18px;
    color: var(--text-muted);
  }

  .meta.problem {
    color: var(--status-error);
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

  .actions {
    flex: none;
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }

  /* In the corner of the screen, like a monitor's power button. */
  .more {
    position: absolute;
    top: var(--space-2);
    right: var(--space-2);
  }

  .power {
    position: relative;
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 1px solid var(--overlay-border);
    border-radius: var(--radius-sm);
    background: rgb(0 0 0 / 45%);
    color: var(--overlay-text);
    cursor: pointer;
  }

  .power:hover,
  .power[aria-expanded='true'] {
    background: var(--overlay-bg-hover);
  }

  .power:focus-visible {
    outline-color: var(--overlay-accent);
  }

  .menu {
    position: absolute;
    top: calc(100% + var(--space-1));
    right: 0;
    z-index: 2;
    display: flex;
    flex-direction: column;
    min-width: 200px;
    padding: var(--space-1);
    border-radius: var(--radius);
    background: var(--surface);
    box-shadow: var(--shadow-float);
    color: var(--text);
  }

  .menu button {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-height: 34px;
    padding: 0 var(--space-3);
    border: 0;
    border-radius: var(--radius-sm);
    background: none;
    text-align: left;
    cursor: pointer;
  }

  .menu button :global(svg) {
    color: var(--text-muted);
  }

  .menu button:hover,
  .menu button:focus-visible {
    background: var(--surface-hover);
  }

  .menu button:focus-visible {
    outline-offset: -2px;
  }

  .separator {
    height: 1px;
    margin: var(--space-1) calc(-1 * var(--space-1));
    background: var(--border);
  }

  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  .problem,
  .empty,
  .find-pc {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-3);
    max-width: 52ch;
  }

  .empty p {
    margin: 0;
  }

  .find {
    display: flex;
    gap: var(--space-2);
    width: 100%;
  }

  .nearby {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-2);
    margin-bottom: var(--space-3);
  }

  .nearby-status {
    margin: 0 0 var(--space-2);
  }

  input {
    flex: 1;
    min-width: 0;
    min-height: 36px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border-control);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: inherit;
    font: inherit;
  }

  input:focus-visible {
    border-color: var(--focus);
    outline-offset: 0;
  }

  @media (pointer: coarse) {
    input {
      min-height: var(--touch-target);
    }

    /* A larger place to touch without covering more of the screen. */
    .power::after {
      content: '';
      position: absolute;
      inset: -10px;
    }

    .menu button {
      min-height: var(--touch-target);
    }
  }

  @media (max-width: 560px) {
    main {
      padding: var(--space-6) var(--space-4);
      padding-top: max(var(--space-6), env(safe-area-inset-top));
    }

    /* Rows: a small screen beside its label, so a phone shows several PCs at once. */
    .devices {
      gap: var(--space-4);
    }

    li {
      display: grid;
      grid-template-columns: minmax(0, 2fr) minmax(0, 3fr);
      align-items: center;
      gap: var(--space-3);
    }

    .screen {
      border-radius: var(--radius-sm);
    }

    /* Out over the label: the small screen is narrower than the menu. */
    .menu {
      right: auto;
      left: 0;
    }

    /* A question needs the whole width. */
    li:has(.confirm) .screen {
      grid-column: 1 / -1;
    }

    .plate {
      flex-direction: column;
      align-items: stretch;
      gap: var(--space-2);
    }

    .standby :global(svg) {
      width: 20px;
      height: 20px;
    }

    .osd {
      padding: var(--space-2);
      font-size: var(--text-sm);
    }

    .confirm {
      padding: var(--space-4);
      font-size: var(--text-md);
    }
  }
</style>
