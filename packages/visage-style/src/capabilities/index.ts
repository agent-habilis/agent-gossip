/**
 * What the engine in front of us actually supports, measured by parsing.
 *
 * Not `CSS.supports()`. It answers a different question than it appears to —
 * happy-dom returns `true` for `zzz: qqq` — so a feature check built on it
 * reports capability it does not have, which is worse than reporting none. The
 * reliable question is "did the rule survive being parsed", and the only way to
 * ask it is to parse one and count what came back.
 *
 * The same probe serves two audiences that would otherwise need separate code
 * paths: a browser too old for `@scope`, and a test runner with a partial CSSOM.
 */

export interface Caps {
  /** `@scope` — required. Without it there is no scoping and no fallback. */
  readonly scope: boolean
  /** `@layer` — degrades to unlayered rules, which merely lose their politeness. */
  readonly layers: boolean
  /** Native nesting. Unused by the compiler, which flattens; probed for the record. */
  readonly nesting: boolean
}

const CANARY =
  '@layer p{.a{color:red}}' +
  '@scope{:scope{color:red}}' +
  '.c{color:red;&:hover{color:blue}}'

/** Assumed when there is no CSSOM to ask — SSR emits the modern form. */
const ASSUMED: Caps = { scope: true, layers: true, nesting: true }

export function probe(): Caps {
  if (typeof document === 'undefined' || typeof CSSStyleSheet !== 'function') return ASSUMED

  let sheet: CSSStyleSheet
  try {
    sheet = new CSSStyleSheet()
    sheet.replaceSync(CANARY)
  } catch {
    return { scope: false, layers: false, nesting: false }
  }

  const rules = [...sheet.cssRules]
  const named = (name: string): boolean => rules.some((r) => r.constructor.name === name)

  // The rule surviving proves nothing about nesting: happy-dom keeps `.c` and
  // drops every declaration that follows the nested block. The declaration is
  // the thing to count.
  const c = rules.find(
    (r): r is CSSStyleRule => (r as CSSStyleRule).selectorText === '.c',
  )

  return {
    scope: named('CSSScopeRule'),
    layers: named('CSSLayerBlockRule'),
    nesting: c !== undefined && c.style.length === 1,
  }
}

let cached: Caps | null = null

export function caps(): Caps {
  cached ??= probe()
  return cached
}

/** Testing seam: forget the probe so the next `caps()` measures again. */
export function resetCaps(): void {
  cached = null
}
