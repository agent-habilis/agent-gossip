/**
 * Exact CSS text, not snapshots.
 *
 * The emitted string is the whole product — it is what the browser parses, what
 * DevTools shows, and what engines deduplicate on. Asserting it literally means
 * a change to the output has to be typed out deliberately rather than accepted
 * by re-recording, which is the point.
 *
 * Both capabilities are forced on. happy-dom parses and applies `@scope` but has
 * no `@layer`, so left to the probe this file would assert the degraded output
 * and never exercise the layered one. `index.test.ts` does the opposite and lets
 * the probe decide, which is what covers the degradation itself.
 */

import { test, expect, beforeAll } from 'bun:test'
import { compile, configure } from './index.ts'

beforeAll(() => {
  configure({ scope: true, layers: true, layer: 'components' })
})

test('wraps declarations in a layer and a scope', () => {
  expect(compile({ display: 'flex', gap: 8 })).toBe(
    '@layer components{@scope{:scope{display:flex;gap:8px;}}}',
  )
})

test('hyphenates camelCase and passes custom properties through', () => {
  expect(compile({ '--accent': 'red', backgroundColor: 'var(--accent)' })).toBe(
    '@layer components{@scope{:scope{--accent:red;background-color:var(--accent);}}}',
  )
})

test('suffixes bare numbers with px, except where the property is unitless', () => {
  expect(compile({ padding: 8, opacity: 0.5, zIndex: 2, margin: 0, '--n': 4 })).toBe(
    '@layer components{@scope{:scope{padding:8px;opacity:0.5;z-index:2;margin:0;--n:4;}}}',
  )
})

test('flex bare numbers stay unitless', () => {
  expect(compile({ flex: 1 })).toBe(
    '@layer components{@scope{:scope{flex:1;}}}',
  )
})

test('compounds an & block onto the current subject', () => {
  expect(compile({ padding: 8, '&:hover': { opacity: 0.5 } })).toBe(
    '@layer components{@scope{:scope{padding:8px;}:scope:hover{opacity:0.5;}}}',
  )
})

test('treats any other selector key as a descendant', () => {
  expect(compile({ 'input[type=checkbox]': { accentColor: 'red' } })).toBe(
    '@layer components{@scope{:scope input[type=checkbox]{accent-color:red;}}}',
  )
})

test('wraps an at-rule around the same subject', () => {
  expect(compile({ '@media (min-width: 40rem)': { padding: 16 } })).toBe(
    '@layer components{@scope{@media (min-width: 40rem){:scope{padding:16px;}}}}',
  )
})

test('nests selectors and at-rules to any depth', () => {
  const css = compile({
    color: 'red',
    '@media (min-width: 40rem)': {
      '&:hover': { color: 'blue' },
      a: { color: 'green' },
    },
  })
  expect(css).toBe(
    '@layer components{@scope{:scope{color:red;}' +
      '@media (min-width: 40rem){:scope:hover{color:blue;}:scope a{color:green;}}}}',
  )
})

test('emits declarations in source order, because the cascade breaks ties on it', () => {
  expect(compile({ margin: 1, padding: 2 })).toBe(
    '@layer components{@scope{:scope{margin:1px;padding:2px;}}}',
  )
  expect(compile({ padding: 2, margin: 1 })).toBe(
    '@layer components{@scope{:scope{padding:2px;margin:1px;}}}',
  )
})

test('skips undefined, so a conditional key can be left off', () => {
  expect(compile({ padding: 8, color: undefined })).toBe(
    '@layer components{@scope{:scope{padding:8px;}}}',
  )
})

test('emits no empty rule when a level only holds nested blocks', () => {
  expect(compile({ '&:hover': { color: 'red' } })).toBe(
    '@layer components{@scope{:scope:hover{color:red;}}}',
  )
})

test('caches on the identity of the style object', () => {
  const style = { padding: 8, color: 'red' }
  expect(compile(style)).toBe(compile(style))
})

test('omits the layer when configured away', () => {
  configure({ layer: null })
  expect(compile({ color: 'red' })).toBe('@scope{:scope{color:red;}}')
  configure({ layer: 'components' })
})

test('drops the layer where @layer is unsupported, keeping the scope', () => {
  // An engine that does not know `@layer` discards the block and everything in
  // it, so keeping the wrapper would cost the styles entirely. Losing the layer
  // costs only its politeness.
  configure({ layers: false })
  expect(compile({ color: 'blue' })).toBe('@scope{:scope{color:blue;}}')
  configure({ layers: true })
})

test('emits nothing at all without @scope, rather than styling the document root', () => {
  // `:scope` outside a scope block matches `:root`, so the "obvious" fallback of
  // dropping the wrapper would move every component's padding onto <html>.
  configure({ scope: false })
  expect(compile({ padding: 8 })).toBe('')
  configure({ scope: true })
})
