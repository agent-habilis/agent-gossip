/**
 * Design tokens as JavaScript bindings, backed by CSS custom properties.
 *
 *     export const T = tokens({ unit: '8px', accent: '#4ea1ff' })
 *
 *     T.unit    // 'var(--unit)' at runtime, `Token<'length'>` to the checker
 *     T.accent  // 'var(--accent)',           `Token<'color'>`
 *
 * The string `'var(--unit)'` told you nothing: not whether the property exists,
 * not what kind of value it holds, and not who else uses it. A module binding
 * answers all three with machinery that already exists — imports, autocomplete,
 * rename, and "find references". Nothing about the *output* changes, so the
 * cascade, theming and runtime overrides work exactly as they did.
 *
 * The kind is inferred from the declared value, which is why declaring the value
 * here rather than in a stylesheet matters: `'8px'` is a length, `'#4ea1ff'` is a
 * colour, `'150ms'` is a time, and anything the grammar does not recognise is
 * `raw` and goes anywhere a string goes.
 *
 * This is `createThemeContract` and `defineVars` without the build step. The
 * difference in cost is that those know the token set at compile time and can
 * drop unused ones; this cannot, so an unused token still ships its declaration.
 */

import { createElement } from 'visage-dom/element'
import type { ElementNode } from 'visage-dom/types'
import type { Angle, Color, KindType, Length, Time, Token } from '../values/index.ts'

/** The token map's values, kept off the string keys so a token cannot shadow them. */
const VALUES: unique symbol = Symbol('visage.style.tokens')

/**
 * Which kind a declared value belongs to.
 *
 * Order is the whole design. `Color` is tested before `Length` because a bare
 * `0` and the colour keywords would otherwise be ambiguous, and `raw` is last
 * because it accepts everything — a shorthand like `'1px solid red'` matches no
 * single grammar and should stay a string rather than be forced into one.
 */
export type KindOf<V> = V extends number
  ? 'number'
  : V extends Color
    ? 'color'
    : V extends Length
      ? 'length'
      : V extends Time
        ? 'time'
        : V extends Angle
          ? 'angle'
          : 'raw'

export type TokenMap<T> = {
  readonly [K in keyof T]: Token<KindOf<T[K]>>
} & {
  readonly [VALUES]: T
}

/** `fgFaint` → `--fg-faint`, the same rule the compiler uses for properties. */
function customProperty(name: string): string {
  return `--${name.replace(/[A-Z]/g, (m) => `-${m.toLowerCase()}`)}`
}

export function tokens<const T extends Record<string, string | number>>(
  values: T,
): TokenMap<T> {
  const map: Record<string, unknown> = { [VALUES]: values }
  for (const name in values) map[name] = `var(${customProperty(name)})`
  return map as TokenMap<T>
}

/**
 * An override has to match its token's *kind*, not its declared literal.
 *
 * `Partial<T>` looks right and is not: `tokens()` infers through a `const` type
 * parameter, so `bg: '#0e1116'` has the literal type `'#0e1116'` and a theme
 * could only ever re-set it to the value it already had. Mapping back through
 * `KindType` restores the grammar — a colour token takes any colour, and only a
 * colour.
 */
export type Overrides<T> = {
  readonly [K in keyof T]?: KindType[KindOf<T[K]> & keyof KindType]
}

export interface ThemeOptions<T> {
  /** Where the custom properties land. `:root` unless you are theming a subtree. */
  readonly selector?: string
  /** Values to override. Omitted tokens keep the ones they were declared with. */
  readonly values?: Overrides<T>
}

/**
 * The `<style>` element that gives the tokens their values.
 *
 * Global on purpose, and the one place this library emits an unscoped rule:
 * custom properties have to be set somewhere an entire document inherits from,
 * which is the opposite of what `@scope` is for. Render it once, near the root.
 *
 * A second call with a different `selector` is a theme — the tokens keep their
 * names, so nothing that *reads* them changes:
 *
 *     Theme(T)
 *     Theme(T, { selector: '.dark', values: { bg: '#000' } })
 */
export function Theme<T extends Record<string, string | number>>(
  map: TokenMap<T>,
  options: ThemeOptions<T> = {},
): ElementNode<'style'> {
  const values: Record<string, string | number> = { ...map[VALUES], ...options.values }
  let body = ''
  for (const name in values) body += `${customProperty(name)}:${String(values[name])};`
  return createElement('style', {}, [`${options.selector ?? ':root'}{${body}}`])
}
