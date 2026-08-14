/**
 * The probe is only worth having if it is honest about the environment it runs
 * in, so this asserts what it reports here rather than what we wish it reported.
 *
 * happy-dom is a partial CSSOM. Whatever it answers, the useful property is that
 * the answer is *measured* — the same code path that decides a 2019 browser has
 * no `@scope` is the one that decides the test runner has none, so there is no
 * separate test-only branch to rot.
 */

import { test, expect } from 'bun:test'
import { probe, caps, resetCaps } from './index.ts'

test('reports a shape with all three capabilities as booleans', () => {
  const result = probe()
  expect(typeof result.scope).toBe('boolean')
  expect(typeof result.layers).toBe('boolean')
  expect(typeof result.nesting).toBe('boolean')
})

test('reports what happy-dom actually has, which is @scope but not @layer', () => {
  // Measured. `@layer` is absent, and a layer block an engine cannot parse takes
  // the scoped rules inside it down with it — which is why `compile` asks before
  // emitting one.
  //
  // The `@scope` answer needs a caveat that this probe cannot express: happy-dom
  // parses the rule and resolves declarations out of it, but ignores the
  // boundary, so scoped rules match the whole document. The probe measures
  // whether a rule *survives parsing*, which is the right signal for the
  // browsers this decision is actually about — an engine without `@scope` drops
  // it outright. It is the wrong signal for a partial CSSOM that keeps the rule
  // and forgets what it means, and no cheap probe distinguishes those.
  expect(probe()).toEqual({ scope: true, layers: false, nesting: false })
})

test('CSS.supports is unreliable here, which is why the probe exists', () => {
  // Documents the trap rather than the fix: this returning true is exactly why
  // feature detection cannot be built on it.
  expect(CSS.supports('zzz', 'qqq')).toBe(true)
})

test('caches the result and can be reset for tests', () => {
  const first = caps()
  expect(caps()).toBe(first)
  resetCaps()
  expect(caps()).toEqual(first)
})
