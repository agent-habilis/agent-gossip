/**
 * The typing is a hard requirement, and these are its proof.
 *
 * Every `@ts-expect-error` below fails the build if it *stops* erroring, so
 * `bun run typecheck` enforces them with no extra tooling. That direction
 * matters: an ordinary test can only show that valid input compiles, and the
 * whole value of the type layer is what it rejects.
 *
 * The load-bearing case is the unknown property. `StyleInput` carries an index
 * signature — it has to, or `raw()` could never reach an unlisted property — and
 * an index signature that swallowed typos would give up most of what typing is
 * for. `CheckStyle` is what stops it, and the fourth test is the one that says
 * so.
 */

import { test, expect } from 'bun:test'
import { Style, raw, tokens } from '../index.ts'

const T = tokens({ unit: '8px', accent: '#4ea1ff' })

test('accepts declarations the table knows', () => {
  const node = Style({
    display: 'flex',
    gap: 8,
    padding: '8px',
    color: 'CanvasText',
    opacity: 0.5,
    zIndex: 2,
    transitionDuration: '150ms',
    background: 'none',
  })
  expect(node.tag).toBe('style')
})

test('rejects a value of the wrong kind', () => {
  Style({
    // @ts-expect-error — opacity is a number, not a colour
    opacity: 'red',
  })
  Style({
    // @ts-expect-error — a bare '8' has no unit and is not a length
    padding: '8',
  })
  Style({
    // @ts-expect-error — 'flx' is not a display keyword
    display: 'flx',
  })
  Style({
    // @ts-expect-error — padding takes no 'auto'
    padding: 'auto',
  })
})

test('rejects an unknown property with a plain value', () => {
  Style({
    // @ts-expect-error — a typo, and the index signature must not swallow it
    paddng: '8px',
  })
})

test('accepts an unknown property through raw()', () => {
  const node = Style({
    padding: '8px',
    anchorName: raw('--tip'),
    positionArea: raw('block-start'),
  })
  expect(node.tag).toBe('style')
})

test('raw() also overrides a known property the grammar cannot express', () => {
  // The shorthand case, and the one that turned up first in real use: `padding`
  // takes one to four lengths in CSS and exactly one in the grammar.
  const node = Style({ padding: raw('3px 8px') })
  expect(node.children[0]).toContain('padding:3px 8px;')
})

test('rejects raw() on a value that is not a literal', () => {
  const computed: string = 'block-start'
  Style({
    // @ts-expect-error — a varying value would defeat the per-object cache
    positionArea: raw(computed),
  })
})

test('accepts custom properties, nested blocks and at-rules', () => {
  const node = Style({
    '--accent': 'oklch(70% 0.2 250)',
    color: T.accent,
    '&:hover': { opacity: 0.8 },
    'input[type=checkbox]': { accentColor: T.accent },
    '@media (min-width: 40rem)': { padding: 16 },
  })
  expect(node.tag).toBe('style')
})

test('a token carries its kind, so it cannot land in the wrong slot', () => {
  const node = Style({ gap: T.unit, color: T.accent })
  expect(node.children[0]).toContain('gap:var(--unit);')

  Style({
    // @ts-expect-error — a colour is not a length
    gap: T.accent,
  })
  Style({
    // @ts-expect-error — and a length is not a colour
    color: T.unit,
  })
})

test('rejects a bare var(), which is what tokens replace', () => {
  Style({
    // @ts-expect-error — nothing checks that `--accent` exists or holds a colour
    color: 'var(--accent)',
  })
  // `raw()` is still the way out when you mean it.
  expect(Style({ color: raw('var(--whatever)') }).tag).toBe('style')
})

test('checks declarations inside nested blocks too', () => {
  Style({
    '&:hover': {
      // @ts-expect-error — nesting does not turn the checking off
      opacity: 'red',
    },
  })
})
