import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { Divider } from './Divider.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/** The compiled stylesheet this instance carries. */
const sheet = (el: Element): string => el.querySelector('style')!.textContent!

test('renders a div with the separator role, never an hr', () => {
  render(<Divider />, host)
  // An <hr> is a void element, so it could not carry the scoped <style> child.
  expect(host.querySelector('hr')).toBeNull()

  const el = host.querySelector('div')!
  expect(el.getAttribute('role')).toBe('separator')
  expect(el.getAttribute('aria-orientation')).toBe('horizontal')
  expect(el.dataset['orientation']).toBe('horizontal')
  expect(el.dataset['weight']).toBe('line')
})

test('horizontal occupies one row and centres the line inside it', () => {
  render(<Divider />, host)
  const css = sheet(host.querySelector('div')!)
  expect(css).toContain('height:calc(1 * var(--ms-row))')
  expect(css).toContain('align-items:center')
  // A string, not a bare 1 — `flex: 1` would compile to `flex:1px`.
  expect(css).toContain('flex:1;')
  expect(css).toContain('border-top:1px solid var(--ms-divider-color)')
})

test('vertical is one cell wide and stretches to its row', () => {
  render(<Divider orientation="vertical" />, host)
  const el = host.querySelector('div')!
  expect(el.getAttribute('aria-orientation')).toBe('vertical')

  const css = sheet(el)
  expect(css).toContain(':scope[data-orientation="vertical"]{')
  expect(css).toContain('width:1ch')
  expect(css).toContain('align-self:stretch')
  expect(css).toContain('border-left:1px solid var(--ms-divider-color)')
})

test('the colour role resolves to a custom property, not a hex', () => {
  render(<Divider color="danger" />, host)
  const el = host.querySelector('div')!
  expect(el.style.getPropertyValue('--ms-divider-color')).toBe('var(--danger)')
  expect(el.getAttribute('style')).not.toMatch(/#[0-9a-f]{6}/i)
})

test('the colour defaults to the border role', () => {
  render(<Divider />, host)
  expect(host.querySelector('div')!.style.getPropertyValue('--ms-divider-color')).toBe(
    'var(--border)',
  )
})

test('weight is a data attribute, so one stylesheet serves every combination', () => {
  render(
    [
      Divider({ weight: 'double' }),
      Divider({ weight: 'thick', orientation: 'vertical' }),
      Divider({ color: 'accent' }),
    ],
    host,
  )
  const [first, second, third] = [...host.querySelectorAll(':scope > div')]
  expect(first!.getAttribute('data-weight')).toBe('double')
  expect(second!.getAttribute('data-weight')).toBe('thick')
  expect(third!.getAttribute('data-weight')).toBe('line')

  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(3)
  expect(new Set(sheets).size).toBe(1)
})

test('the weight mapping keeps double as a real CSS border style', () => {
  render(<Divider weight="double" />, host)
  const css = sheet(host.querySelector('div')!)
  // 3px double is two hairlines with a gap — the analogue of '═'.
  expect(css).toContain('border-top:3px double var(--ms-divider-color)')
  expect(css).toContain('border-top:2px solid var(--ms-divider-color)')
})

test('a caller style prop survives alongside the colour property', () => {
  render(<Divider style={{ opacity: '0.5' }} />, host)
  const el = host.querySelector('div')! as HTMLElement
  expect(el.style.opacity).toBe('0.5')
  expect(el.style.getPropertyValue('--ms-divider-color')).toBe('var(--border)')
})

test('caller attrs merge with the role rather than replacing it', () => {
  render(<Divider attrs={{ 'aria-label': 'section break' }} />, host)
  const el = host.querySelector('div')!
  expect(el.getAttribute('role')).toBe('separator')
  expect(el.getAttribute('aria-label')).toBe('section break')
})
