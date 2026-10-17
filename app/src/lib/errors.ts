/** An error written for people: what happened and what to do, with the raw cause kept for "Details". */
export class Problem extends Error {
  constructor(
    message: string,
    readonly details?: string,
  ) {
    super(message)
  }
}

/** Anything thrown, as a Problem. Errors not written for people get `fallback` as their message. */
export function toProblem(e: unknown, fallback: string): Problem {
  if (e instanceof Problem) return e
  if (e && typeof e === 'object' && 'message' in e && 'details' in e) {
    // Shape returned by the app's own commands.
    const { message, details } = e as { message: string; details?: string }
    return new Problem(message, details)
  }
  const raw = e instanceof Error ? e.message : String(e)
  return new Problem(fallback, raw || undefined)
}
