/**
 * Style object → the text of one `<style>` element.
 *
 * The output is a prelude-less `@scope` block, which the browser scopes to the
 * `<style>` element's own parent. That is the entire scoping mechanism: no
 * class, no hash, no attribute, no registry, and so nothing that can collide.
 *
 * Selectors are flattened rather than nested. Native nesting would express the
 * same thing in fewer bytes, but flattening costs nothing at runtime, reads
 * unambiguously in DevTools, and sidesteps a real portability question — engines
 * disagree about declarations that follow a nested block, and happy-dom drops
 * them outright. Every emitted selector starts at `:scope` for the same reason:
 * inside `@scope` a bare `img` already means `:scope img`, and writing it out
 * removes the need to know that.
 */

import { DEV } from 'visage-dom/dev'
import { caps } from '../capabilities/index.ts'
import { isUnitless } from '../props/index.ts'
import type { StyleNode } from '../values/index.ts'
import { validate } from '../validate/index.ts'

let layer = 'components'
let forceScope: boolean | null = null
let forceLayers: boolean | null = null

export interface Config {
  /**
   * Cascade layer for every generated rule. A layer is politeness: layered
   * rules lose to un-layered author CSS, so an app's own stylesheet wins
   * without `!important`. Pass `null` to emit no layer at all.
   */
  readonly layer?: string | null
  /**
   * Override the `@scope` probe. The only reason to touch it is testing the
   * degraded path, which is why it is not inferred from anything.
   */
  readonly scope?: boolean
  /** Override the `@layer` probe. Same reasoning as `scope`. */
  readonly layers?: boolean
}

/**
 * Must run before the first `Style()` call — compiled text is cached per style
 * object, so anything already compiled keeps the settings it compiled under.
 */
export function configure(config: Config): void {
  if (config.layer !== undefined) layer = config.layer ?? ''
  if (config.scope !== undefined) forceScope = config.scope
  if (config.layers !== undefined) forceLayers = config.layers
}

function hyphenate(key: string): string {
  return key.startsWith('--') ? key : key.replace(/[A-Z]/g, (m) => `-${m.toLowerCase()}`)
}

/**
 * A bare number is a length everywhere except the handful of properties that
 * take unitless numbers, which is why `props.ts` records a kind at all. Zero
 * needs no unit and reads better without one.
 *
 * `flex` is typed `raw` (it also accepts `1 1 auto`), but a bare number is the
 * common grow shorthand and must stay unitless — `flex: 1px` collapses layout.
 */
function serialize(prop: string, value: string | number): string {
  if (typeof value !== 'number') return value
  if (value === 0) return '0'
  if (prop === 'flex') return String(value)
  return isUnitless(prop) ? String(value) : `${value}px`
}

/**
 * One nesting level.
 *
 * `for...in` yields string keys in insertion order, so the emitted CSS follows
 * the order it was written — which matters, because within one selector the
 * cascade breaks ties by order of appearance.
 */
function block(node: StyleNode, selector: string): string {
  let decls = ''
  let nested = ''

  for (const key in node) {
    const value = node[key]
    if (value === undefined || value === null) continue

    if (typeof value === 'object') {
      if (key.charCodeAt(0) === 64) {
        // '@' — an at-rule: same subject, wrapped.
        nested += `${key}{${block(value, selector)}}`
      } else if (key.charCodeAt(0) === 38) {
        // '&' — compounds onto the current subject: `&:hover` → `:scope:hover`.
        nested += block(value, selector + key.slice(1))
      } else {
        nested += block(value, `${selector} ${key}`)
      }
      continue
    }

    const prop = hyphenate(key)
    const serialized = serialize(key, value)
    if (DEV) validate(prop, serialized)
    decls += `${prop}:${serialized};`
  }

  return (decls === '' ? '' : `${selector}{${decls}}`) + nested
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

/**
 * Keyed on the style object's identity, which is why the documented shape is a
 * module-level const: hoisted, a component type compiles once no matter how many
 * instances mount. An inline literal is a fresh object per instance and
 * recompiles, so DEV says so.
 */
const CACHE = new WeakMap<object, string>()

/** DEV only: first object seen for a given output, to catch inline literals. */
const SEEN = DEV ? new Map<string, object>() : null

/** DEV only: the missing-`@scope` warning is worth saying once, not per component. */
let warnedNoScope = false

export function compile(node: StyleNode): string {
  const cached = CACHE.get(node)
  if (cached !== undefined) return cached

  const body = block(node, ':scope')
  const useScope = forceScope ?? caps().scope

  // Without `@scope` there is nothing to fall back to, and emitting the rules
  // anyway would be worse than emitting none. Every other scoping scheme names
  // its root — a hashed class, a data attribute — and the point of this one is
  // that the root is *unnamed*, so there is no selector to prefix with. Outside
  // a `@scope` block `:scope` matches the document root, so the fallback would
  // silently move every component's padding onto `<html>`.
  //
  // Unstyled and loud beats styled and wrong.
  if (!useScope) {
    if (DEV && !warnedNoScope) {
      warnedNoScope = true
      console.warn(
        'visage-style: this browser has no @scope, so no styles were emitted.\n' +
          '  @scope is Baseline newly available (Chrome 118, Safari 17.4,\n' +
          '  Firefox 146) and this library requires it.',
      )
    }
    CACHE.set(node, '')
    return ''
  }

  // An engine that does not understand `@layer` drops the whole block, taking
  // the scoped rules inside it with it — so an unsupported layer is not a
  // cosmetic loss, it is total. Dropping the wrapper costs only the politeness
  // the layer was buying, which is the right thing to give up.
  const useLayer = layer !== '' && (forceLayers ?? caps().layers)
  const text = useLayer ? `@layer ${layer}{@scope{${body}}}` : `@scope{${body}}`

  CACHE.set(node, text)

  // `DEV &&` rather than the `SEEN !== null` narrowing alone: the bundler folds
  // a `DEV` guard directly but does not propagate the folded value through the
  // `const` above into this test, so on its own the branch — and the paragraph
  // of advice inside it — survives into production. `scripts/build.test.ts`
  // caught that leak and is what keeps it caught.
  if (DEV && SEEN !== null) {
    const first = SEEN.get(text)
    if (first === undefined) {
      SEEN.set(text, node)
    } else if (first !== node) {
      console.warn(
        'visage-style: recompiled an identical style object.\n' +
          '  Hoist it to a module-level const so it compiles once per component\n' +
          '  type rather than once per instance:\n' +
          '    const ROW = { padding: 8 }\n' +
          '    …\n' +
          '    Style(ROW)\n' +
          `  Rule: ${text.slice(0, 120)}${text.length > 120 ? '…' : ''}`,
      )
    }
  }

  return text
}
