import { raw } from 'visage-style'
import { t } from './tokens.ts'

/**
 * Grid math and the shared style fragments, as plain objects.
 *
 * The rule these enforce is unchanged from the styled-components version:
 * horizontal measurements are in `ch` (exact in a monospace font), vertical
 * measurements are in whole rows, and nothing is a raw pixel.
 *
 * What changed is the shape. A `css` tagged template interpolated at render
 * time; these are frozen objects spread into a style declaration at author
 * time. Composition reads as `{ ...FOCUS_RING, ...INVERSE }` instead of two
 * interpolations whose order was implicit.
 */

/**
 * `n` cells of horizontal space.
 *
 * The return type is spelled out rather than inferred. TypeScript widens an
 * interpolated template literal to `string`, and `string` is not a `Length` —
 * the grammar matches on `` `${number}${LengthUnit}` ``, so the annotation is
 * what keeps this assignable to a width.
 */
export const cells = (n: number): `${number}ch` => `${n}ch`

/**
 * `n` rows of vertical space.
 *
 * A `calc()` rather than a token reference, and that is what makes it general:
 * the grammar admits `calc()` on every kind through `Dynamic`, so this one
 * helper serves `height` (a length) and `lineHeight` (a number), which a
 * `Token<'length'>` could not.
 */
export const rows = (n: number): `calc(${number} * var(--ms-row))` =>
  `calc(${n} * var(--ms-row))`

/**
 * A single-row control: exactly one row tall, with the text centred by the line
 * box rather than by padding, so it lands on the baseline grid.
 */
export const ONE_ROW = {
  height: rows(1),
  lineHeight: rows(1),
} as const

/**
 * Inverse video — swap foreground and background.
 *
 * The system's primary emphasis mechanism, not a decorative one. It is the only
 * highlight that survives a 16-colour or monochrome terminal intact, which is
 * why selection, focus and active states all reach for it before hue.
 */
export const INVERSE = {
  background: t.fg,
  color: t.bg,
} as const

/**
 * Focus ring.
 *
 * `outlineOffset: 0` keeps the ring flush against the cell boundary instead of
 * bleeding into the neighbouring cell. Always paired with a glyph or an
 * inversion change at the component level, so focus is never signalled by
 * colour alone.
 */
export const FOCUS_RING = {
  outline: `var(--ms-border-width) solid ${t.accent}`,
  outlineOffset: 0,
} as const

/** Applied to disabled controls. Dims without hiding, and blocks pointer events. */
export const DISABLED = {
  color: t.fgSubtle,
  borderColor: t.border,
  cursor: 'not-allowed',
  pointerEvents: 'none',
} as const

/**
 * Visually hidden but still announced by screen readers. Used to pair a real
 * `<input>` with its glyph rendering — the native control stays in the
 * accessibility tree and keeps keyboard behaviour, while the visible affordance
 * is a character.
 */
export const SR_ONLY = {
  position: 'absolute',
  width: '1px',
  height: '1px',
  padding: 0,
  margin: '-1px',
  overflow: 'hidden',
  // `clipPath` is not in the grammar's property table, so it goes through
  // `raw()`. That gives up static checking, not checking — DEV still runs the
  // declaration past `CSS.supports`.
  clipPath: raw('inset(50%)'),
  whiteSpace: 'nowrap',
  border: 0,
} as const

/**
 * Snap a percentage width down to a whole number of cells, so a container never
 * ends mid-character and its children stay on the lattice.
 *
 * `round()` is not among the functions the value grammar knows
 * (`calc`/`min`/`max`/`clamp`/`anchor-size`), so it goes through `raw()`. The
 * `@supports` guard keeps browsers without it merely imperfect rather than
 * broken, and an `@`-prefixed key compiles to a real `@supports` block wrapping
 * the same subject.
 */
export const SNAP = {
  width: '100%',
  '@supports (width: round(down, 100%, 1ch))': {
    width: raw('round(down, 100%, 1ch)'),
  },
} as const

/**
 * `SNAP`, with the default measure as a ceiling.
 *
 * Two constants rather than the old `snapWidth(max?)` function: a function
 * returns a fresh object per call, which misses the compile cache keyed on
 * object identity and trips the DEV warning about recompiling identical CSS.
 * These were its only two shapes in practice.
 */
export const SNAP_MEASURE = {
  width: '100%',
  maxWidth: t.msMeasure,
  '@supports (width: round(down, 100%, 1ch))': {
    width: raw('round(down, 100%, 1ch)'),
    maxWidth: raw('min(var(--ms-measure), round(down, 100%, 1ch))'),
  },
} as const
