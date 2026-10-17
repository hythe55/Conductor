import type { PowerAction, PowerSchedule } from './types'

export const EVERY_DAY = [0, 1, 2, 3, 4, 5, 6]
export const WEEKDAYS = [1, 2, 3, 4, 5]
export const WEEKENDS = [0, 6]

const same = (a: number[], b: number[]) => a.length === b.length && a.every((d) => b.includes(d))

/** Monday, Tuesday… for day 0 (Sunday) to 6. In English, like the rest of the app. */
export function weekday(day: number, style: 'long' | 'short') {
  // 2023-01-01 was a Sunday.
  return new Intl.DateTimeFormat('en-US', { weekday: style }).format(new Date(2023, 0, 1 + day))
}

/** "every day", "on weekdays", "on Mondays", "on Mon, Thu". */
export function daysText(days: number[]) {
  if (same(days, EVERY_DAY)) return 'every day'
  if (same(days, WEEKDAYS)) return 'on weekdays'
  if (same(days, WEEKENDS)) return 'on weekends'
  if (days.length === 1) return `on ${weekday(days[0], 'long')}s`
  return `on ${[...days].sort().map((d) => weekday(d, 'short')).join(', ')}`
}

/** Minutes after midnight as the reader's clock shows them, e.g. "1:00 AM". */
export function timeText(at: number) {
  return new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' }).format(
    new Date(2023, 0, 1, Math.floor(at / 60), at % 60),
  )
}

const does: Record<PowerAction, string> = { sleep: 'sleeps', restart: 'restarts', shutDown: 'shuts down' }
const doing: Record<PowerAction, string> = { sleep: 'Sleep', restart: 'Restart', shutDown: 'Shut down' }

/** "Sleep every day at 1:00 AM". */
export function describe(s: PowerSchedule) {
  return `${doing[s.action]} ${daysText(s.days)} at ${timeText(s.at)}`
}

/** For a PC's line in the device list: "sleeps every day at 1:00 AM". */
export function summary(list: PowerSchedule[]) {
  if (list.length === 0) return null
  if (list.length > 1) return `${list.length} scheduled actions`
  const [s] = list
  return `${does[s.action]} ${daysText(s.days)} at ${timeText(s.at)}`
}
