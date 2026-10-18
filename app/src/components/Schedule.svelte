<script lang="ts">
  import { untrack } from 'svelte'
  import { setSchedule, type HostUrl } from '../lib/api'
  import { Problem, toProblem } from '../lib/errors'
  import { describe, EVERY_DAY, weekday, WEEKDAYS, WEEKENDS } from '../lib/schedule'
  import type { PowerAction, PowerSchedule } from '../lib/types'
  import Icon from './Icon.svelte'
  import ProblemText from './ProblemText.svelte'

  let {
    id,
    host,
    name,
    schedule,
    onchange,
    onclose,
  }: {
    /** Unique on the page, for labels. */
    id: string
    host: HostUrl
    name: string
    schedule: PowerSchedule[]
    onchange: (schedule: PowerSchedule[]) => void
    onclose: () => void
  } = $props()

  const MAX = 8
  const dayChoices: { label: string; days: number[] }[] = [
    { label: 'Every day', days: EVERY_DAY },
    { label: 'Weekdays', days: WEEKDAYS },
    { label: 'Weekends', days: WEEKENDS },
    ...EVERY_DAY.map((d) => ({ label: `${weekday(d, 'long')}s`, days: [d] })),
  ]

  // Changes show here at once, while the device list may take a few seconds to catch up.
  let list = $state(untrack(() => schedule))
  let action = $state<PowerAction>('sleep')
  let dayChoice = $state(0)
  let time = $state('01:00')
  let saving = $state(false)
  let problem = $state<Problem | null>(null)

  async function save(next: PowerSchedule[]) {
    if (saving) return
    saving = true
    problem = null
    try {
      list = await setSchedule(host, next)
      onchange(list)
    } catch (e) {
      problem = toProblem(e, `Couldn’t change ${name}’s schedule.`)
    }
    saving = false
  }

  async function add(e: SubmitEvent) {
    e.preventDefault()
    const [h, m] = time.split(':').map(Number)
    if (!Number.isFinite(h) || !Number.isFinite(m)) return
    const entry: PowerSchedule = { action, at: h * 60 + m, days: dayChoices[dayChoice].days }
    if (list.some((s) => describe(s) === describe(entry))) {
      problem = new Problem(`${describe(entry)} is already scheduled.`)
      return
    }
    await save([...list, entry])
    // The form goes away once the list is full.
    if (list.length >= MAX) document.getElementById(`schedule-done-${id}`)?.focus()
  }

  function remove(i: number) {
    save(list.filter((_, j) => j !== i))
    // The button that was pressed goes away.
    document.getElementById(`schedule-done-${id}`)?.focus()
  }

  const icons = { sleep: 'sleep', restart: 'restart', shutDown: 'power' } as const

  let dialog: HTMLDialogElement
  $effect(() => {
    dialog.showModal()
  })

  // Escape closes it.
  function oncancel(e: Event) {
    e.preventDefault()
    onclose()
  }
</script>

<dialog bind:this={dialog} aria-labelledby="schedule-{id}" {oncancel}>
  <h2 id="schedule-{id}">Schedule for {name}</h2>
  <p class="quiet">If someone is using {name} at that time, it waits until they stop, for up to an hour.</p>

  {#if list.length}
    <ul>
      {#each list as s, i (describe(s))}
        <li>
          <span class="icon"><Icon name={icons[s.action]} /></span>
          <span class="what">{describe(s)}</span>
          <button class="btn quiet" aria-label="Remove: {describe(s)}" onclick={() => remove(i)}>Remove</button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="nothing">Nothing is scheduled.</p>
  {/if}

  {#if list.length < MAX}
    <form onsubmit={add}>
      <select bind:value={action} aria-label="Action" data-first>
        <option value="sleep">Sleep</option>
        <option value="restart">Restart</option>
        <option value="shutDown">Shut down</option>
      </select>
      <select bind:value={dayChoice} aria-label="Days">
        {#each dayChoices as c, i (c.label)}
          <option value={i}>{c.label}</option>
        {/each}
      </select>
      <input type="time" bind:value={time} required aria-label="Time" />
      <button class="btn">Add</button>
    </form>
  {:else}
    <p class="quiet">That’s as many as a PC can have. Remove one to add another.</p>
  {/if}

  {#if problem}<div class="problem" role="alert"><ProblemText {problem} /></div>{/if}
  <div class="buttons">
    <button class="btn primary" id="schedule-done-{id}" onclick={onclose}>Done</button>
  </div>
</dialog>

<style>
  dialog {
    width: min(480px, calc(100vw - 2 * var(--space-4)));
    max-height: calc(100dvh - 2 * var(--space-4));
    overflow: auto;
    padding: var(--space-6);
    border: 0;
    border-radius: var(--radius-lg);
    background: var(--surface);
    box-shadow: var(--shadow-float);
    color: var(--text);
  }

  dialog::backdrop {
    background: rgb(8 12 11 / 45%);
  }

  h2 {
    margin: 0 0 var(--space-1);
    font-family: var(--font-name);
    font-size: var(--text-lg);
    font-stretch: 88%;
    font-weight: 650;
    line-height: 1.25;
  }

  p {
    margin: 0;
  }

  .quiet {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }

  .nothing {
    margin: var(--space-4) 0;
    color: var(--text-muted);
  }

  ul {
    margin: var(--space-4) 0;
    padding: 0;
    list-style: none;
    border-top: 1px solid var(--border);
  }

  li {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-height: 44px;
    border-bottom: 1px solid var(--border);
  }

  .icon {
    color: var(--text-muted);
  }

  .what {
    flex: 1;
    min-width: 0;
  }

  li .btn {
    margin-right: calc(-1 * var(--space-3));
    color: var(--text-muted);
  }

  li .btn:hover {
    color: var(--text);
  }

  form {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    margin-bottom: var(--space-4);
  }

  select,
  input {
    min-height: 36px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border-control);
    border-radius: var(--radius-sm);
    background: var(--bg);
    color: inherit;
    font: inherit;
  }

  select {
    flex: 1 1 8em;
  }

  @media (pointer: coarse) {
    select,
    input {
      min-height: var(--touch-target);
    }
  }

  .problem {
    margin-bottom: var(--space-4);
    color: var(--status-error);
  }

  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
