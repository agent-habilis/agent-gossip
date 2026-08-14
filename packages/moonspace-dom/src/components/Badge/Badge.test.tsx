import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { Badge } from './Badge.tsx'

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

test('renders a span, solid and neutral by default', () => {
  render(<Badge>ready</Badge>, host)
  const el = host.querySelector('span')!
  expect(label(el)).toBe('ready')
  expect(el.dataset['variant']).toBe('solid')
  expect(el.dataset['tone']).toBe('neutral')
  expect(el.style.getPropertyValue('--ms-badge-tone')).toBe('var(--fg-muted)')
})

test('the tone resolves to a semantic role, not a hex', () => {
  render(<Badge tone="success">ready</Badge>, host)
  const el = host.querySelector('span')!
  expect(el.style.getPropertyValue('--ms-badge-tone')).toBe('var(--success)')
  // A hex here would mean the component had forked from the theme.
  expect(el.getAttribute('style')).not.toMatch(/#[0-9a-f]{6}/i)
})

test('one property drives the text colour and the solid fill', () => {
  render(<Badge tone="danger">error</Badge>, host)
  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain('color:var(--ms-badge-tone)')
  expect(sheet).toContain('[data-variant="solid"]{')
  expect(sheet).toContain('background:var(--ms-badge-tone)')
  // Reverse video: the fill is the tone, the label is the page background.
  expect(sheet).toContain('color:var(--bg)')
})

test('outline brackets the label with two literal characters', () => {
  render(<Badge variant="outline" tone="warning">building</Badge>, host)
  const el = host.querySelector('span')!
  expect(el.dataset['variant']).toBe('outline')

  const sheet = el.querySelector('style')!.textContent!
  expect(sheet).toContain("::before{content:'‹';}")
  expect(sheet).toContain("::after{content:'›';}")
  // A border would participate in layout and inflate the line box the badge
  // sits in, which is why the brackets are characters.
  expect(sheet).not.toContain('border')
})

test('the badge is one row tall and uppercase', () => {
  render(<Badge>ready</Badge>, host)
  const sheet = host.querySelector('style')!.textContent!
  expect(sheet).toContain('height:calc(1 * var(--ms-row))')
  expect(sheet).toContain('text-transform:uppercase')
  // Uppercase costs no extra width in a monospace font, so the badge stays on
  // whole cells with a cell of padding each side.
  expect(sheet).toContain('padding-inline:1ch')
})

test('six tones and two variants share one stylesheet', () => {
  render(
    [
      Badge({ tone: 'accent', children: 'preview' }),
      Badge({ tone: 'info', variant: 'outline', children: 'cached' }),
    ],
    host,
  )
  const [solid, outline] = [...host.querySelectorAll('span')]
  expect(solid!.style.getPropertyValue('--ms-badge-tone')).toBe('var(--accent)')
  expect(outline!.style.getPropertyValue('--ms-badge-tone')).toBe('var(--info)')

  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(2)
  expect(new Set(sheets).size).toBe(1)
})

test('a caller style prop survives alongside the tone property', () => {
  render(<Badge style={{ marginLeft: '2ch' }}>queued</Badge>, host)
  const el = host.querySelector('span')!
  expect(el.style.marginLeft).toBe('2ch')
  expect(el.style.getPropertyValue('--ms-badge-tone')).toBe('var(--fg-muted)')
})
