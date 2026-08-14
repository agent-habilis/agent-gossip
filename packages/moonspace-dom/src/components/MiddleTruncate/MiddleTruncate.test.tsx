import { afterEach, beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { MiddleTruncate } from './MiddleTruncate.tsx'

/*
 * The measuring path cannot be asserted here, and no test pretends otherwise.
 * happy-dom has no layout: every `getBoundingClientRect()` is zero-width, so
 * the probe's advance width is zero, `measure` bails out, and the budget stays
 * null. A test written against that would pass for the wrong reason. What is
 * covered instead is the `budget` escape hatch, which is the same arithmetic
 * without the measurement, the untruncated fallback, and the teardown.
 */

let host: HTMLElement

/** Records what the component does with a ResizeObserver, since layout cannot. */
class SpyResizeObserver {
  static observed = 0
  static disconnected = 0
  observe(): void {
    SpyResizeObserver.observed += 1
  }
  unobserve(): void {}
  disconnect(): void {
    SpyResizeObserver.disconnected += 1
  }
}

const real = globalThis.ResizeObserver

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
  SpyResizeObserver.observed = 0
  SpyResizeObserver.disconnected = 0
  globalThis.ResizeObserver = SpyResizeObserver as unknown as typeof ResizeObserver
})

afterEach(() => {
  globalThis.ResizeObserver = real
})

/** The rendered text, excluding the scoped <style> element's own contents. */
const label = (el: Element): string =>
  [...el.childNodes]
    .filter((n) => n.nodeName !== 'STYLE')
    .map((n) => n.textContent)
    .join('')

test('an explicit budget truncates in the middle, with no measurement at all', () => {
  const root = render(<MiddleTruncate value="~/dev/personal/moonspace/Button.tsx" budget={20} />, host)
  const el = host.querySelector('span')!

  expect(label(el)).toBe('~/dev/pers…utton.tsx')
  expect([...label(el)]).toHaveLength(20)
  // The head keeps the odd character: leading context is what disambiguates a path.
  expect(SpyResizeObserver.observed).toBe(0)

  root.unmount()
})

test('a value inside the budget is left alone', () => {
  const root = render(<MiddleTruncate value="deploy" budget={20} />, host)
  expect(label(host.querySelector('span')!)).toBe('deploy')
  root.unmount()
})

test('the full value stays reachable on title', () => {
  const value = 'sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08'
  const root = render(<MiddleTruncate value={value} budget={16} />, host)
  const el = host.querySelector('span')!

  expect(el.title).toBe(value)
  expect(label(el)).not.toBe(value)
  root.unmount()
})

test('without a budget it observes, renders the full value until a measurement lands, and disconnects on unmount', () => {
  const root = render(<MiddleTruncate value="~/dev/personal/moonspace/Button.tsx" />, host)
  const el = host.querySelector('span')!

  expect(SpyResizeObserver.observed).toBe(1)
  // Zero-width here, so the budget never leaves null — which is exactly the
  // pre-measurement state a real browser passes through on the first frame.
  expect(label(el)).toBe('~/dev/personal/moonspace/Button.tsx')

  root.unmount()
  expect(SpyResizeObserver.disconnected).toBe(1)
  expect(host.innerHTML.replace(/<!---->/g, '')).toBe('')
})
