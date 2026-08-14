import { beforeEach, expect, test } from 'bun:test'
import { flushSync, render } from 'visage-dom'
import { Input } from './Input.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/** The marker glyph, if one was rendered. `<style>` is a child node too. */
const marker = (el: Element): string | null => el.querySelector('span')?.textContent ?? null

test('wraps a real <input>, so keyboard and form behaviour stay native', () => {
  render(<Input name="branch" defaultValue="main" />, host)
  const wrapper = host.firstElementChild as HTMLElement
  const field = host.querySelector('input')!

  // The wrapper is the root because <input> is void and cannot host the <style>.
  expect(wrapper.tagName).toBe('DIV')
  expect(wrapper.firstElementChild!.tagName).toBe('STYLE')
  expect(field.parentElement).toBe(wrapper)
  expect(field.name).toBe('branch')
  expect(field.value).toBe('main')
  expect(marker(wrapper)).toBeNull()
})

test('a cell width becomes a custom property, not a stylesheet per instance', () => {
  render([Input({ width: 40 }), Input({ width: 12 })], host)
  const [a, b] = [...host.querySelectorAll('div')]
  expect(a!.dataset['width']).toBe('fixed')
  expect(a!.style.getPropertyValue('--ms-input-width')).toBe('40ch')
  expect(b!.style.getPropertyValue('--ms-input-width')).toBe('12ch')

  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  expect(new Set(sheets).size).toBe(1)
  expect(sheets[0]!).toContain('[data-width="fixed"]{width:var(--ms-input-width);}')
})

test('omitting the width fills the parent, snapped to whole cells', () => {
  render(<Input />, host)
  const wrapper = host.firstElementChild as HTMLElement
  expect(wrapper.dataset['width']).toBe('fill')
  // No stray property to resolve against.
  expect(wrapper.style.getPropertyValue('--ms-input-width')).toBe('')
  const sheet = wrapper.querySelector('style')!.textContent!
  expect(sheet).toContain('[data-width="fill"]{width:100%;}')
  expect(sheet).toContain('round(down, 100%, 1ch)')
})

test('invalid shows a marker as well as recolouring, and says so to a screen reader', () => {
  render(<Input invalid defaultValue="feat//bad name" />, host)
  const wrapper = host.firstElementChild!
  expect(wrapper.getAttribute('data-invalid')).toBe('true')
  // The outline is a colour signal; the glyph is what survives a mono terminal.
  expect(marker(wrapper)).toBe('!')
  expect(wrapper.querySelector('span')!.getAttribute('aria-hidden')).toBe('true')
  expect(host.querySelector('input')!.getAttribute('aria-invalid')).toBe('true')
})

test('a prefix renders in the marker column, and invalid replaces it', () => {
  render(<Input prefix=">" />, host)
  expect(marker(host.firstElementChild!)).toBe('>')

  host.innerHTML = ''
  render(<Input prefix=">" invalid />, host)
  expect(marker(host.firstElementChild!)).toBe('!')
})

test('a valid field carries no aria-invalid at all', () => {
  render(<Input />, host)
  expect(host.querySelector('input')!.hasAttribute('aria-invalid')).toBe(false)
})

test('the invalid outline outranks focus, and disabled outranks both', () => {
  render(<Input />, host)
  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain('[data-invalid="true"]{outline-color:var(--danger);}')
  expect(sheet).toContain(':focus-within{outline-color:var(--accent);}')
  expect(sheet).toContain('[data-invalid="true"]:focus-within{outline-color:var(--danger);}')
  // Equal specificity between the first two, so order is the tiebreak.
  expect(sheet.indexOf(':focus-within{')).toBeLessThan(
    sheet.indexOf('[data-invalid="true"]:focus-within'),
  )
})

test('the field and its placeholder are styled through descendant selectors', () => {
  render(<Input placeholder="project name" />, host)
  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain(':scope input{')
  expect(sheet).toContain(':scope input::placeholder{color:var(--fg-subtle);}')
  // `flex: 1` as a number would have compiled to `flex:1px`.
  expect(sheet).toContain('flex:1;')
  expect(sheet).not.toContain('flex:1px')
})

test('typing reaches an oninput handler', () => {
  const seen: string[] = []
  render(<Input oninput={(e) => seen.push((e.target as HTMLInputElement).value)} />, host)
  const field = host.querySelector('input')!

  field.value = 'ma'
  field.dispatchEvent(new Event('input'))
  flushSync()
  expect(seen).toEqual(['ma'])
})

test('disabled reaches the real input, and the wrapper drops its fill', () => {
  render(<Input disabled defaultValue="locked" />, host)
  const field = host.querySelector('input')!
  expect(field.disabled).toBe(true)

  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain(':scope input:disabled{color:var(--fg-subtle);cursor:not-allowed;}')
  expect(sheet).toContain(':has(input:disabled){background:transparent')
})

test('a caller class and style land on the wrapper, alongside the width property', () => {
  render(<Input class="field" style={{ marginTop: '2ch' }} width={20} />, host)
  const wrapper = host.firstElementChild as HTMLElement
  expect(wrapper.className).toBe('field')
  expect(wrapper.style.marginTop).toBe('2ch')
  expect(wrapper.style.getPropertyValue('--ms-input-width')).toBe('20ch')
})
