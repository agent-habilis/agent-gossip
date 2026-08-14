import { beforeEach, expect, test } from 'bun:test'
import { flushSync, render } from 'visage-dom'
import { Checkbox } from './Checkbox.tsx'

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

test('the control is a real checkbox input, not a painted div', () => {
  render(<Checkbox>ship it</Checkbox>, host)
  const input = host.querySelector('input')!
  expect(input.type).toBe('checkbox')
  expect(label(host.querySelector('label')!)).toBe('[ ]ship it')
})

test('the marker is the adjacent sibling of the input, with the style ahead of both', () => {
  render(<Checkbox>ship it</Checkbox>, host)
  const el = host.querySelector('label')!

  // The `+` selectors below only match while these two are adjacent, and the
  // <style> is an ordinary element that would sit between them if it went last.
  expect(el.firstElementChild!.tagName).toBe('STYLE')
  const input = host.querySelector('input')!
  expect(input.nextElementSibling!.tagName).toBe('SPAN')
})

test('the sibling selectors survive compilation', () => {
  render(<Checkbox />, host)
  const sheet = host.querySelector('style')!.textContent!

  /*
   * The failure this pins down is silent: `&` is substituted only at the start
   * of a key, so the original's `input:focus-visible + &` would compile to a
   * literal `&` and be thrown away by the parser with no error anywhere.
   */
  expect(sheet).toContain(':scope input:focus-visible + span{')
  expect(sheet).toContain(':scope input:checked + span{color:var(--accent);}')
  expect(sheet).toContain(':scope input:disabled + span{color:var(--fg-subtle);}')
  expect(sheet).not.toContain('+ &')
})

test('focus inverts the marker rather than drawing a ring', () => {
  render(<Checkbox />, host)
  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain(':scope input:focus-visible + span{background:var(--fg);color:var(--bg);}')
})

test('checked draws [x] and checks the native input', () => {
  render(<Checkbox checked>ship it</Checkbox>, host)
  expect(host.querySelector('input')!.checked).toBe(true)
  expect(host.querySelector('span')!.textContent).toBe('[×]')
})

test('indeterminate draws [-] and sets the platform mixed state', () => {
  render(<Checkbox indeterminate>ship it</Checkbox>, host)
  const input = host.querySelector('input')!
  expect(host.querySelector('span')!.textContent).toBe('[-]')
  // Mixed is a display state: the input itself stays unchecked.
  expect(input.indeterminate).toBe(true)
  expect(input.checked).toBe(false)

  // Setting the real property is what lets `:indeterminate` do the colouring;
  // React's `aria-checked="mixed"` could not reach it.
  expect(host.querySelector('style')!.textContent).toContain(
    ':scope input:indeterminate + span{color:var(--accent);}',
  )
})

test('disabled dims the whole label, not only the marker', () => {
  render(<Checkbox disabled>ship it</Checkbox>, host)
  expect(host.querySelector('input')!.disabled).toBe(true)
  expect(host.querySelector('style')!.textContent).toContain(
    ':scope:has(input:disabled){color:var(--fg-subtle);cursor:not-allowed;}',
  )
})

test('onchange fires with the native event', () => {
  let seen: boolean | undefined
  render(<Checkbox onchange={(e) => (seen = (e.target as HTMLInputElement).checked)} />, host)

  const input = host.querySelector('input')!
  input.click()
  flushSync()

  expect(seen).toBe(true)
})

test('class and style land on the label, not on the hidden input', () => {
  render(<Checkbox class="row" style={{ width: '20ch' }} />, host)
  const el = host.querySelector('label')!
  expect(el.className).toBe('row')
  expect(el.style.width).toBe('20ch')
  expect(host.querySelector('input')!.className).toBe('')
})

test('two instances share one compiled stylesheet', () => {
  render([Checkbox({ children: 'a' }), Checkbox({ children: 'b' })], host)
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  expect(new Set(sheets).size).toBe(1)
})
