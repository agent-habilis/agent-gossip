/**
 * Tokens are mostly a type-level device, so the runtime surface is small: a map
 * of `var(…)` strings and a `<style>` element that gives them values. The
 * checking they buy is asserted in `types.test.ts`.
 */

import { test, expect } from 'bun:test'
import { Theme, tokens } from './index.ts'

test('a token is the var() reference for its hyphenated name', () => {
  const t = tokens({ unit: '8px', fgFaint: '#55616f', panel2: '#1c2230' })
  expect(String(t.unit)).toBe('var(--unit)')
  expect(String(t.fgFaint)).toBe('var(--fg-faint)')
  // One hyphenation rule, no per-token exceptions: `panel2` is `--panel2`, and
  // the stylesheet was renamed to match rather than the rule bent to fit it.
  expect(String(t.panel2)).toBe('var(--panel2)')
})

test('Theme emits the declared values against :root', () => {
  const t = tokens({ unit: '8px', accent: '#4ea1ff' })
  expect(Theme(t).children[0]).toBe(':root{--unit:8px;--accent:#4ea1ff;}')
})

test('Theme is the one unscoped rule, because inheritance is the point', () => {
  // Custom properties have to be set somewhere the whole document inherits
  // from, which is the opposite of what `@scope` does. No `@scope`, no `@layer`.
  const css = String(Theme(tokens({ bg: '#000' })).children[0])
  expect(css).not.toContain('@scope')
  expect(css).not.toContain('@layer')
})

test('a theme overrides some values and keeps the rest', () => {
  const t = tokens({ bg: '#0e1116', fg: '#c9d4e3' })
  expect(Theme(t, { selector: '.light', values: { bg: '#fff' } }).children[0]).toBe(
    '.light{--bg:#fff;--fg:#c9d4e3;}',
  )
})

test('token names survive theming, so nothing that reads them changes', () => {
  const t = tokens({ bg: '#0e1116' })
  const dark = Theme(t, { selector: '.dark', values: { bg: '#000' } })
  expect(String(t.bg)).toBe('var(--bg)')
  expect(String(dark.children[0])).toContain('--bg:#000')
})

test('numbers serialize without a unit, since a token value is literal', () => {
  const t = tokens({ ratio: 1.5 })
  expect(Theme(t).children[0]).toBe(':root{--ratio:1.5;}')
})
