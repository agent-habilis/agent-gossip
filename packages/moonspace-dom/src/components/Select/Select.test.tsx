import { beforeEach, expect, test } from 'bun:test'
import { flushSync, render } from 'visage-dom'
import { Select } from './Select.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

const OPTIONS = [
  { value: 'staging', label: 'staging' },
  { value: 'prod', label: 'production' },
  { value: 'edge', label: 'edge', disabled: true },
]

test('the control is a real select, wrapped rather than replaced', () => {
  render(<Select options={OPTIONS} />, host)
  const select = host.querySelector('select')!
  expect(select).not.toBeNull()

  // <select> is not a valid parent for a <style>, which is why the scope root is
  // the wrapper and every rule below reaches the control as a descendant.
  expect(select.querySelector('style')).toBeNull()
  expect(host.querySelector('div')!.firstElementChild!.tagName).toBe('STYLE')
})

test('options render in order, carrying value and disabled', () => {
  render(<Select options={OPTIONS} />, host)
  const options = [...host.querySelectorAll('option')]

  expect(options.map((o) => o.value)).toEqual(['staging', 'prod', 'edge'])
  expect(options.map((o) => o.textContent)).toEqual(['staging', 'production', 'edge'])
  expect(options[2]!.disabled).toBe(true)
})

test('a placeholder is a disabled first option, not a separate mechanism', () => {
  render(<Select options={OPTIONS} placeholder="pick one" />, host)
  const first = host.querySelector('option')!
  expect(first.textContent).toBe('pick one')
  expect(first.value).toBe('')
  expect(first.disabled).toBe(true)
})

test('the chevron is ours, drawn in the last cell and hidden from AT', () => {
  render(<Select options={OPTIONS} />, host)
  const chevron = host.querySelector('span')!
  expect(chevron.textContent).toBe('▾')
  expect(chevron.getAttribute('aria-hidden')).toBe('true')

  const sheet = host.querySelector('style')!.textContent!
  // The platform's own chevron is stripped: it is a vector at an arbitrary size
  // and would be the only thing on screen not sitting in a cell.
  expect(sheet).toContain('appearance:none;')
  expect(sheet).toContain(':scope span{flex:none;width:2ch;text-align:right;')
})

test('descendant selectors reach the control and its options', () => {
  render(<Select options={OPTIONS} />, host)
  const sheet = host.querySelector('style')!.textContent!

  expect(sheet).toContain(':scope select{')
  expect(sheet).toContain(':scope select:disabled{cursor:not-allowed;}')
  expect(sheet).toContain(':scope option{background:var(--bg-sunken);color:var(--fg);}')
  // `flex: 1` would have serialised as `1px`; the string spelling is the fix.
  expect(sheet).toContain('flex:1;')
})

test('no width fills the parent, snapped down to whole cells', () => {
  render(<Select options={OPTIONS} />, host)
  const el = host.querySelector('div')!
  expect(el.dataset['fixed']).toBe('false')
  expect(el.style.getPropertyValue('--ms-select-width')).toBe('')

  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain('width:100%;')
  expect(sheet).toContain('@supports (width: round(down, 100%, 1ch))')
})

test('a width in cells arrives as a custom property, and out-specifies the fill rule', () => {
  render(<Select options={OPTIONS} width={24} />, host)
  const el = host.querySelector('div')!
  expect(el.dataset['fixed']).toBe('true')
  expect(el.style.getPropertyValue('--ms-select-width')).toBe('24ch')

  // `[data-fixed]` beats the bare `:scope` that SNAP writes, inside the
  // @supports guard as well as outside it — otherwise round(down, 100%, 1ch)
  // would win on source order.
  expect(host.querySelector('style')!.textContent).toContain(
    ':scope[data-fixed="true"]{width:var(--ms-select-width);}',
  )
})

test('disabled empties the well and dims the label through :has', () => {
  render(<Select options={OPTIONS} disabled />, host)
  expect(host.querySelector('select')!.disabled).toBe(true)
  expect(host.querySelector('style')!.textContent).toContain(
    ':scope:has(select:disabled){background:transparent;color:var(--fg-subtle);}',
  )
})

test('focus lights the wrapper outline, since the wrapper is what reads as the field', () => {
  render(<Select options={OPTIONS} />, host)
  expect(host.querySelector('style')!.textContent).toContain(
    ':scope:focus-within{outline-color:var(--accent);}',
  )
})

test('onchange fires with the native event', () => {
  let seen: string | undefined
  render(
    <Select
      options={OPTIONS}
      onchange={(e) => (seen = (e.target as HTMLSelectElement).value)}
    />,
    host,
  )

  const select = host.querySelector('select')!
  select.value = 'prod'
  select.dispatchEvent(new Event('change'))
  flushSync()

  expect(seen).toBe('prod')
})

test('class and style land on the wrapper, alongside the width property', () => {
  render(<Select options={OPTIONS} width={12} class="field" style={{ opacity: '0.5' }} />, host)
  const el = host.querySelector('div')!
  expect(el.className).toBe('field')
  expect(el.style.opacity).toBe('0.5')
  expect(el.style.getPropertyValue('--ms-select-width')).toBe('12ch')
})

test('two instances share one compiled stylesheet', () => {
  render([Select({ options: OPTIONS }), Select({ options: OPTIONS, width: 30 })], host)
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  // The per-instance width is a custom property, so the sheet itself is shared.
  expect(new Set(sheets).size).toBe(1)
})
