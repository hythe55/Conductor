<script lang="ts">
  import { untrack } from 'svelte'
  import { setSchedule, type HostUrl } from '../lib/api'
  import { Problem, toProblem } from '../lib/errors'
  import { describe, EVERY_DAY, weekday, WEEKDAYS, WEEKENDS } from '../lib/schedule'
  import type { PowerAction, PowerSchedule } from '../lib/types'
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

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault()
      onclose()
    }
  }
</script>

<!-- Escape from any control inside closes it. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div class="schedule" role="group" aria-labelledby="schedule-{id}" {onkeydown}>
  <p class="title" id="schedule-{id}">Schedule for {name}</p>
  {#if list.length}
    <ul>
      {#each list as s, i (describe(s))}
        <li>
          <span>{describe(s)}</span>
          <button class="link" aria-label="Remove: {describe(s)}" onclick={() => remove(i)}>Remove</button>
        </li>
      {/each}
    </ul>
  {:else}
    <p class="quiet">Nothing is scheduled.</p>
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

  <p class="quiet">If someone is using {name} at that time, it waits until they stop, for up to an hour.</p>
  {#if problem}<div class="problem" role="alert"><ProblemText {problem} /></div>{/if}
  <div class="buttons">
    <button class="btn" id="schedule-done-{id}" onclick={onclose}>Done</button>
  </div>
</div>

<style>
  .schedule {
    grid-column: 2 / -1;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--space-3);
  }

  p {
    margin: 0;
    max-width: 60ch;
  }

  .title {
    font-weight: 600;
  }

  .quiet {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    width: 100%;
  }

  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-4);
    max-width: 60ch;
  }

  form {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }

  select,
  input {
    min-height: 36px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    background: var(--surface);
    color: inherit;
    font: inherit;
  }

  @media (pointer: coarse) {
    select,
    input {
      min-height: var(--touch-target);
    }
  }

  .problem {
    color: var(--status-error);
  }

  .buttons {
    display: flex;
    gap: var(--space-2);
  }
</style>
