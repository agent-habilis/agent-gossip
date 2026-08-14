import { beforeEach, expect, test } from 'bun:test'
import { flushSync, render } from 'visage-dom'
import { Radio } from './Radio.tsx'

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

test('the control is a real radio input', () => {
  render(<Radio name="env">staging</Radio>, host)
  const input = host.querySelector('input')!
  expect(input.type).toBe('radio')
  expect(input.name).toBe('env')
  expect(label(host.querySelector('label')!)).toBe('( )staging')
})

test('parentheses, not brackets — the shape is the semantics', () => {
  render(<Radio checked>staging</Radio>, host)
  expect(host.querySelector('span')!.textContent).toBe('(•)')
  expect(host.querySelector('input')!.checked).toBe(true)
})

test('the marker is the adjacent sibling of the input, with the style ahead of both', () => {
  render(<Radio>staging</Radio>, host)
  const el = host.querySelector('label')!
  expect(el.firstElementChild!.tagName).toBe('STYLE')
  expect(host.querySelector('input')!.nextElementSibling!.tagName).toBe('SPAN')
})

test('the sibling selectors survive compilation', () => {
  render(<Radio />, host)
  const sheet = host.querySelector('style')!.textContent!

  // `&` is only substituted at the start of a key, so `input:checked + &` would
  // compile to literal nonsense and be dropped without an error being raised.
  expect(sheet).toContain(':scope input:focus-visible + span{background:var(--fg);color:var(--bg);}')
  expect(sheet).toContain(':scope input:checked + span{color:var(--accent);}')
  expect(sheet).toContain(':scope input:disabled + span{color:var(--fg-subtle);}')
  expect(sheet).not.toContain('+ &')
})

test('disabled dims the whole label', () => {
  render(<Radio disabled>staging</Radio>, host)
  expect(host.querySelector('input')!.disabled).toBe(true)
  expect(host.querySelector('style')!.textContent).toContain(
    ':scope:has(input:disabled){color:var(--fg-subtle);cursor:not-allowed;}',
  )
})

test('onchange fires with the native event', () => {
  let seen: string | undefined
  render(
    <Radio value="staging" onchange={(e) => (seen = (e.target as HTMLInputElement).value)} />,
    host,
  )

  host.querySelector('input')!.click()
  flushSync()

  expect(seen).toBe('staging')
})

test('a group shares a name, so the platform enforces exactly one', () => {
  render([Radio({ name: 'env', value: 'a' }), Radio({ name: 'env', value: 'b' })], host)
  const [first, second] = [...host.querySelectorAll('input')]

  first!.click()
  second!.click()
  flushSync()

  expect(first!.checked).toBe(false)
  expect(second!.checked).toBe(true)
})

test('two instances share one compiled stylesheet', () => {
  render([Radio({ children: 'a' }), Radio({ children: 'b' })], host)
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(new Set(sheets).size).toBe(1)
})
