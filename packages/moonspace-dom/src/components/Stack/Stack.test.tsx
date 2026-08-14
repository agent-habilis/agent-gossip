import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { Stack } from './Stack.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/** The rendered text, excluding the scoped <style> element's own contents. */
const label = (el: Element): string =>
  [...el.childNodes]
    .filter((n) => n.nodeName !== 'STYLE')
    .map((n) => n.textContent)
    .join('')

test('renders a div, column, with the defaults on data attributes', () => {
  render(<Stack>one</Stack>, host)
  const el = host.querySelector('div')!
  expect(label(el)).toBe('one')
  expect(el.dataset['direction']).toBe('column')
  expect(el.dataset['align']).toBe('stretch')
  expect(el.dataset['justify']).toBe('start')
  expect(el.dataset['wrap']).toBe('false')
})

test('as changes the element without changing the styling', () => {
  render(<Stack as="ul">items</Stack>, host)
  const el = host.querySelector('ul')!
  expect(el).not.toBeNull()
  expect(el.querySelector('style')!.textContent).toContain('display:flex')
})

test('the gap unit follows the axis', () => {
  render([Stack({ direction: 'row', gap: 2 }), Stack({ direction: 'column', gap: 2 })], host)
  const [row, column] = [...host.querySelectorAll('div')]

  // Two columns of space horizontally, two rows of space vertically. One
  // number, one meaning, two units.
  expect(row!.style.getPropertyValue('--ms-stack-gap')).toBe('2ch')
  expect(column!.style.getPropertyValue('--ms-stack-gap')).toBe('calc(2 * var(--ms-row))')
})

test('an unset gap is zero, not absent', () => {
  render(<Stack />, host)
  const el = host.querySelector('div')!
  // An empty custom property would make `gap: var(--ms-stack-gap)` invalid and
  // fall back to the initial value — same result here, but by accident.
  expect(el.style.getPropertyValue('--ms-stack-gap')).toBe('calc(0 * var(--ms-row))')
})

test('align and justify select rules rather than generating them', () => {
  render(<Stack direction="row" align="center" justify="between" wrap />, host)
  const el = host.querySelector('div')!
  expect(el.dataset['align']).toBe('center')
  expect(el.dataset['justify']).toBe('between')
  expect(el.dataset['wrap']).toBe('true')

  const sheet = el.querySelector('style')!.textContent!
  expect(sheet).toContain('[data-align="center"]{align-items:center;}')
  expect(sheet).toContain('[data-justify="between"]{justify-content:space-between;}')
  expect(sheet).toContain('[data-wrap="true"]{flex-wrap:wrap;}')
})

test('snapping turns itself on exactly when alignment can leave the origin', () => {
  render(
    [
      Stack({}),
      Stack({ justify: 'center' }),
      Stack({ align: 'end' }),
      Stack({ align: 'baseline' }),
    ],
    host,
  )
  const [plain, centred, ended, baseline] = [...host.querySelectorAll('div')]

  // Packed from the start edge, the container's own width never positions a
  // child, so there is nothing to snap.
  expect(plain!.dataset['snap']).toBe('false')
  expect(baseline!.dataset['snap']).toBe('false')
  expect(centred!.dataset['snap']).toBe('true')
  expect(ended!.dataset['snap']).toBe('true')
})

test('snap can be forced or suppressed', () => {
  render([Stack({ snap: true }), Stack({ justify: 'end', snap: false })], host)
  const [forced, suppressed] = [...host.querySelectorAll('div')]
  expect(forced!.dataset['snap']).toBe('true')
  expect(suppressed!.dataset['snap']).toBe('false')
})

test('the snap rule is guarded by @supports and sets no width otherwise', () => {
  render(<Stack snap />, host)
  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain('@supports (width: round(down, 100%, 1ch))')
  expect(sheet).toContain('round(down, 100%, 1ch)')
  // Not the SNAP mixin: a Stack must not start filling its parent just because
  // it snaps.
  expect(sheet).not.toContain('width:100%')
})

test('one stylesheet serves every combination', () => {
  render(
    [
      Stack({ direction: 'row', gap: 1, align: 'center' }),
      Stack({ direction: 'column', gap: 4, justify: 'around', wrap: true }),
    ],
    host,
  )
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  expect(new Set(sheets).size).toBe(1)
})

test('children mount after the scoped style', () => {
  render(
    <Stack>
      <span>a</span>
      <span>b</span>
    </Stack>,
    host,
  )
  const el = host.querySelector('div')!
  expect(el.firstElementChild!.tagName).toBe('STYLE')
  expect([...el.querySelectorAll('span')].map((s) => s.textContent)).toEqual(['a', 'b'])
})

test('a caller style prop survives alongside the gap property', () => {
  render(<Stack gap={1} style={{ width: '20ch' }} />, host)
  const el = host.querySelector('div')!
  expect(el.style.width).toBe('20ch')
  expect(el.style.getPropertyValue('--ms-stack-gap')).toBe('calc(1 * var(--ms-row))')
})
