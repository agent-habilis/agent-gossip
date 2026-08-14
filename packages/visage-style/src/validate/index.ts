/**
 * DEV-only declaration checking, by asking the browser.
 *
 * The type layer catches what a curated grammar can catch. This catches the
 * rest — a property outside the table that went through `raw()`, a `calc()` with
 * mismatched units, a value the running browser has not shipped — by handing the
 * declaration to `CSS.supports`, which is the same parser that will decide
 * whether to keep it.
 *
 * That makes the escape hatch checked rather than unchecked: `raw()` gives up
 * *static* verification, not verification. Every call site here is behind `DEV`,
 * so the whole module folds away in a production build.
 *
 * Note this is the opposite use from feature detection. Asking
 * `CSS.supports('zzz', 'qqq')` whether a *feature* exists is unreliable —
 * happy-dom answers `true` — which is why `capabilities.ts` parses a canary
 * instead. Asking whether *this* declaration parses is what the API is for, and
 * a permissive environment merely means the check finds nothing.
 */

/** One warning per distinct declaration; a 1000-row list must not shout 1000 times. */
const WARNED = new Set<string>()

export function validate(prop: string, value: string): void {
  // No CSSOM: SSR, or a test runner without one. Nothing to ask.
  if (typeof CSS === 'undefined' || typeof CSS.supports !== 'function') return

  let ok: boolean
  try {
    ok = CSS.supports(prop, value)
  } catch {
    // A malformed property name can throw rather than return false.
    ok = false
  }
  if (ok) return

  const key = `${prop}:${value}`
  if (WARNED.has(key)) return
  WARNED.add(key)

  console.warn(
    `visage-style: the browser rejected\n` +
      `    ${prop}: ${value}\n` +
      '  A typo, a value this browser has not shipped, or a unit mismatch.\n' +
      '  The declaration will be dropped by the CSS parser.',
  )
}
