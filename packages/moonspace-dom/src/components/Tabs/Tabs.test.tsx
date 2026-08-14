import { beforeEach, expect, test } from 'bun:test'
import { flushSync, render } from 'visage-dom'
import { Tabs } from './Tabs.tsx'
import type { Tab } from './Tabs.tsx'

const tabs: Tab[] = [
  { id: 'overview', label: 'Overview', content: <span>ready</span> },
  { id: 'logs', label: 'Logs', content: <span>streaming</span> },
  { id: 'metrics', label: 'Metrics', disabled: true },
]

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

const triggers = (): HTMLButtonElement[] => [
  ...host.querySelectorAll<HTMLButtonElement>('[role="tab"]'),
]

const arrow = (el: HTMLElement, key: 'ArrowLeft' | 'ArrowRight'): KeyboardEvent => {
  const event = new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true })
  el.dispatchEvent(event)
  flushSync()
  return event
}

test('two instances get distinct ids', () => {
  render([Tabs({ tabs }), Tabs({ tabs })], host)

  // The counter is read in the generator prologue, which runs once per instance
  // — so six tabs point at six panels rather than two sets of the same three.
  const controls = triggers().map((el) => el.getAttribute('aria-controls'))
  expect(controls).toHaveLength(6)
  expect(new Set(controls).size).toBe(6)
})

test('the first tab is selected, and clicking moves the selection', () => {
  render(<Tabs tabs={tabs} />, host)
  const [first, second] = triggers()

  expect(first!.getAttribute('aria-selected')).toBe('true')
  expect(first!.tabIndex).toBe(0)
  expect(second!.tabIndex).toBe(-1)

  second!.click()
  flushSync()

  expect(first!.getAttribute('aria-selected')).toBe('false')
  expect(second!.getAttribute('aria-selected')).toBe('true')
  expect(second!.tabIndex).toBe(0)
  expect(host.querySelector('[role="tabpanel"]')!.textContent).toContain('streaming')
})

test('the arrows move the selection, skipping disabled tabs and wrapping', () => {
  render(<Tabs tabs={tabs} />, host)
  const [first, second] = triggers()

  const event = arrow(first!, 'ArrowRight')
  expect(second!.getAttribute('aria-selected')).toBe('true')
  // Otherwise the page scrolls under the bar.
  expect(event.defaultPrevented).toBe(true)

  // Metrics is disabled, so ArrowRight from Logs wraps past it to Overview.
  arrow(second!, 'ArrowRight')
  expect(first!.getAttribute('aria-selected')).toBe('true')

  arrow(first!, 'ArrowLeft')
  expect(second!.getAttribute('aria-selected')).toBe('true')
})

test('a controlled value overrides the internal state', () => {
  const seen: string[] = []
  render(<Tabs tabs={tabs} value="logs" onValueChange={(id) => seen.push(id)} />, host)
  const [first, second] = triggers()

  expect(second!.getAttribute('aria-selected')).toBe('true')

  first!.click()
  flushSync()

  // The parent was told and did not change `value`, so nothing moved. An
  // internal write here would have left the two disagreeing.
  expect(second!.getAttribute('aria-selected')).toBe('true')
  expect(seen).toEqual(['overview'])
})

test('the panel is labelled by its tab and controlled by it', () => {
  render(<Tabs tabs={tabs} defaultValue="logs" />, host)
  const panel = host.querySelector('[role="tabpanel"]')!
  const tab = triggers()[1]!

  expect(panel.getAttribute('aria-labelledby')).toBe(tab.id)
  expect(tab.getAttribute('aria-controls')).toBe(panel.id)
})

test('a tab with no content renders no panel', () => {
  render(<Tabs tabs={[{ id: 'a', label: 'Overview' }]} />, host)
  expect(host.querySelector('[role="tabpanel"]')).toBeNull()
})
