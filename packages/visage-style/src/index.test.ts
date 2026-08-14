/**
 * Structure, through a real render.
 *
 * These do not force capabilities: whatever `probe()` reports is what runs, so
 * this file exercises the degraded path that `compile.test.ts` forces off.
 *
 * What is deliberately *not* asserted here is scoping itself. happy-dom parses
 * implicit `@scope` into a `CSSScopeRule` and resolves declarations out of it,
 * but it ignores the boundary — a scoped rule matches the whole document, so
 * two components leak into each other and a descendant selector escapes. Tests
 * that appear to prove isolation would therefore pass for the wrong reason
 * today and start failing the day happy-dom implements scoping properly.
 *
 * So the split is: everything mechanical is checked here, and the one claim the
 * library rests on — that a `<style>` inside an element scopes to that element —
 * is checked in a browser against `examples/`. Saying so is better than a green
 * test that means nothing.
 */

import { test, expect, afterEach } from 'bun:test'
import { render } from 'visage-dom'
import { component } from 'visage-dom/element'
import { tags } from 'visage-dom'
import { Style, css, raw } from './index.ts'

const { div, span } = tags

/** Mounted hosts have to reach the document, or `getComputedStyle` sees nothing. */
const mounted: HTMLElement[] = []

function mount(node: Parameters<typeof render>[0]): HTMLElement {
  const host = document.createElement('div')
  document.body.append(host)
  mounted.push(host)
  render(node, host)
  return host
}

// `document` is shared across every test file in the run, so a host left behind
// is a stylesheet left behind, and these are stylesheets that deliberately have
// opinions about `div` and `span`. Leaving them attached made an unrelated
// suite fail.
afterEach(() => {
  for (const host of mounted.splice(0)) host.remove()
})

test('builds a style element carrying the compiled css', () => {
  const node = Style({ color: 'rgb(1, 2, 3)' })
  expect(node.tag).toBe('style')
  expect(node.children[0]).toContain('@scope{:scope{color:rgb(1, 2, 3);}}')
})

test('renders the style element inside its component root', () => {
  const CARD = css({ color: 'rgb(4, 5, 6)', padding: 8 })
  const Card = component(function* () {
    yield () => div({ id: 'card' }, Style(CARD), span({}, 'in'))
  })

  const host = mount(Card({}))
  const card = host.querySelector<HTMLElement>('#card')!
  const style = card.firstElementChild as HTMLStyleElement

  // Position is the whole mechanism: the scope root is this element because the
  // <style> is inside it, not because anything was named or hashed.
  expect(style.tagName).toBe('STYLE')
  expect(style.parentElement).toBe(card)
  expect(style.textContent).toContain('padding:8px;')
})

test('unmounting a component takes its styles with it', () => {
  // The reason `Style` is an element and not a registry: there is no cleanup
  // code here, and so none to get wrong. The reconciler removes a child node.
  const GONE = css({ color: 'rgb(7, 8, 9)' })
  const Toggle = component<{ on: boolean }>(function* (props) {
    yield () => div({}, props.on ? div({}, Style(GONE), span({}, 'x')) : null)
  })

  const host = mount(Toggle({ on: true }))
  expect(host.querySelectorAll('style').length).toBe(1)

  render(Toggle({ on: false }), host)
  expect(host.querySelectorAll('style').length).toBe(0)
})

test('every instance carries the same text, so engines parse it once', () => {
  const ROW = css({ color: 'rgb(11, 12, 13)' })
  const Row = component(function* () {
    yield () => div({}, Style(ROW), span({}, 'row'))
  })

  const host = mount(div({}, Row({}), Row({}), Row({})))
  const texts = [...host.querySelectorAll('style')].map((s) => s.textContent)

  expect(texts.length).toBe(3)
  expect(new Set(texts).size).toBe(1)
})

test('a raw() value reaches the output untouched', () => {
  const node = Style({ padding: 16, anchorName: raw('--tip') })
  expect(node.children[0]).toContain('anchor-name:--tip;')
})
