import { expect, test } from 'bun:test'
import { Style, css } from 'visage-style'
import type { ElementNode } from 'visage-dom/types'
import {
  DISABLED,
  FOCUS_RING,
  INVERSE,
  ONE_ROW,
  SNAP,
  SNAP_MEASURE,
  SR_ONLY,
  cells,
  rows,
} from './mixins.ts'

/*
 * These mixins only meet the value grammar when they are spread into a `css()`
 * call, so a test that merely imports them proves nothing. Every assertion here
 * goes through `css()` + `Style()` and reads the compiled text.
 *
 * Substring assertions throughout, never equality: happy-dom reports no
 * `@layer` support, so it emits `@scope{…}` where Chrome emits
 * `@layer moonspace{@scope{…}}`. An equality snapshot would encode the test
 * environment and break the day happy-dom gains CSSLayerBlockRule.
 */
const compiled = (node: ElementNode<'style'>): string => String(node.children[0])

test('cells and rows keep their units', () => {
  expect(cells(4)).toBe('4ch')
  expect(rows(1)).toBe('calc(1 * var(--ms-row))')
  expect(rows(0)).toBe('calc(0 * var(--ms-row))')
})

test('ONE_ROW drives height and line-height off the same row variable', () => {
  const text = compiled(Style(css({ ...ONE_ROW })))
  expect(text).toContain('height:calc(1 * var(--ms-row));')
  // lineHeight has kind `number`, which admits calc() but not a length token —
  // this is why `rows()` returns a calc string rather than referencing t.msRow.
  expect(text).toContain('line-height:calc(1 * var(--ms-row));')
})

test('INVERSE swaps the two token references', () => {
  expect(compiled(Style(css({ ...INVERSE })))).toContain('background:var(--fg);color:var(--bg);')
})

test('FOCUS_RING sits on the cell boundary', () => {
  const text = compiled(Style(css({ ...FOCUS_RING })))
  expect(text).toContain('outline:var(--ms-border-width) solid var(--accent);')
  expect(text).toContain('outline-offset:0;')
})

test('DISABLED dims and blocks pointers', () => {
  const text = compiled(Style(css({ ...DISABLED })))
  expect(text).toContain('color:var(--fg-subtle);')
  expect(text).toContain('cursor:not-allowed;')
  expect(text).toContain('pointer-events:none;')
})

test('SR_ONLY emits clip-path through raw()', () => {
  const text = compiled(Style(css({ ...SR_ONLY })))
  expect(text).toContain('clip-path:inset(50%);')
  // A bare 0 must not acquire a unit; the compiler leaves a plain 0 alone.
  expect(text).toContain('padding:0;')
})

test('SNAP wraps round() in a @supports block rather than dropping it', () => {
  const text = compiled(Style(css({ ...SNAP })))
  expect(text).toContain('width:100%;')
  expect(text).toContain('@supports (width: round(down, 100%, 1ch))')
  expect(text).toContain('width:round(down, 100%, 1ch);')
})

test('SNAP_MEASURE adds the measure ceiling inside and outside the guard', () => {
  const text = compiled(Style(css({ ...SNAP_MEASURE })))
  expect(text).toContain('max-width:var(--ms-measure);')
  expect(text).toContain('max-width:min(var(--ms-measure), round(down, 100%, 1ch));')
})

test('mixins compose by spread, and later keys win', () => {
  const text = compiled(Style(css({ ...DISABLED, ...INVERSE })))
  // INVERSE's color overrides DISABLED's, because the spread already resolved it —
  // there is one `color` declaration, not two relying on source order.
  expect(text).toContain('color:var(--bg);')
  expect(text).not.toContain('color:var(--fg-subtle);')
})

test('a number lands as px unless the property is unitless', () => {
  // The silent-wrong-output case worth pinning: `flex` has kind `raw`, so a bare
  // 1 would serialise as `1px`. Strings are the only safe spelling.
  expect(compiled(Style(css({ flex: '1' })))).toContain('flex:1;')
  expect(compiled(Style(css({ padding: 8 })))).toContain('padding:8px;')
})
