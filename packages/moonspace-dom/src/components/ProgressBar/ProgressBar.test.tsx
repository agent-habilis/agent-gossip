import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { ProgressBar } from './ProgressBar.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

const root = (): HTMLElement => host.querySelector('span')!
const part = (name: string): HTMLElement => host.querySelector(`[data-part="${name}"]`)!

test('the track is real text, in whole cells', () => {
  render(<ProgressBar label="build" value={0.5} width={10} />, host)
  // Ten cells, half of them filled. The string is exactly what a terminal prints.
  expect(part('track').textContent).toBe('█████░░░░░')
  expect(part('fill').textContent).toBe('█████')
})

test('a fraction of a cell rounds rather than pretending to sub-cell precision', () => {
  render(<ProgressBar label="build" value={0.67} width={20} />, host)
  // 0.67 * 20 = 13.4 cells.
  expect(part('fill').textContent).toHaveLength(13)
  expect(part('track').textContent).toHaveLength(20)
  expect(part('value').textContent).toBe('67%')
})

test('values outside 0..1 are clamped', () => {
  render(<ProgressBar label="build" value={2} width={8} />, host)
  expect(part('fill').textContent).toBe('████████')
  expect(root().getAttribute('aria-valuenow')).toBe('100')

  document.body.innerHTML = ''
  const other = document.createElement('div')
  document.body.append(other)
  render(<ProgressBar label="build" value={-1} width={8} />, other)
  expect(other.querySelector('[data-part="fill"]')!.textContent).toBe('')
  expect(other.querySelector('span')!.getAttribute('aria-valuenow')).toBe('0')
})

test('the ARIA is complete and every value is a string attribute', () => {
  render(<ProgressBar label="deploy" value={0.25} />, host)
  const el = root()
  expect(el.getAttribute('role')).toBe('progressbar')
  expect(el.getAttribute('aria-label')).toBe('deploy')
  expect(el.getAttribute('aria-valuenow')).toBe('25')
  expect(el.getAttribute('aria-valuemin')).toBe('0')
  expect(el.getAttribute('aria-valuemax')).toBe('100')
  // The drawing is decoration; the value reaches AT through ARIA alone.
  expect(part('track').getAttribute('aria-hidden')).toBe('true')
})

test('a caller attrs entry is merged rather than replaced', () => {
  render(<ProgressBar label="deploy" value={0.25} attrs={{ 'aria-describedby': 'hint' }} />, host)
  expect(root().getAttribute('aria-describedby')).toBe('hint')
  expect(root().getAttribute('role')).toBe('progressbar')
})

test('tone and value are per-instance custom properties', () => {
  render(<ProgressBar label="build" value={0.4} tone="danger" />, host)
  expect(root().style.getPropertyValue('--ms-progress-tone')).toBe('var(--danger)')
  expect(root().style.getPropertyValue('--ms-progress-value')).toBe('40%')
  expect(root().getAttribute('style')).not.toMatch(/#[0-9a-f]{6}/i)
})

test('showValue drops the number, and the track alone remains', () => {
  render(<ProgressBar label="build" value={0.4} showValue={false} />, host)
  expect(host.querySelector('[data-part="value"]')).toBeNull()
  expect(part('track')).not.toBeNull()
})

test('fluid measures the container instead of taking a width', () => {
  // Vendor-local prop. Layout does not run under happy-dom, so the probe
  // measures zero and the bar draws nothing — what is observable is the
  // wiring: the fluid flag reaches the DOM and an observer is attached.
  class SpyResizeObserver {
    static observed = 0
    observe(): void {
      SpyResizeObserver.observed += 1
    }
    disconnect(): void {}
  }
  const real = globalThis.ResizeObserver
  globalThis.ResizeObserver = SpyResizeObserver as unknown as typeof ResizeObserver
  try {
    render(<ProgressBar label="copy" value={0.5} fluid />, host)
    expect(root().dataset['fluid']).toBe('true')
    expect(SpyResizeObserver.observed).toBe(1)
    expect(part('track').textContent).toBe('')
  } finally {
    globalThis.ResizeObserver = real
  }
})

test('one stylesheet serves every tone and width', () => {
  render(
    [
      ProgressBar({ label: 'a', value: 0.1, tone: 'success' }),
      ProgressBar({ label: 'b', value: 0.9, tone: 'warning', width: 8 }),
    ],
    host,
  )
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  expect(new Set(sheets).size).toBe(1)

  expect(sheets[0]).toContain('color:var(--ms-progress-tone)')
  // The track must not be allowed to collapse or wrap its run of characters.
  expect(sheets[0]).toContain('white-space:pre')
})
