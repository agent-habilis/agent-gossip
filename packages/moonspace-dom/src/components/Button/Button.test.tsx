import { beforeEach, expect, test } from 'bun:test'
import { flushSync, render } from 'visage-dom'
import { Button } from './Button.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/** The rendered label, excluding the scoped <style> element's own contents. */
const label = (el: Element): string =>
  [...el.childNodes]
    .filter((n) => n.nodeName !== 'STYLE')
    .map((n) => n.textContent)
    .join('')

test('renders a real <button>, defaulting to type="button" and the secondary variant', () => {
  render(<Button>deploy</Button>, host)
  const el = host.querySelector('button')!
  expect(label(el)).toBe('deploy')
  // Not `submit`: an unqualified Button inside a form must not submit it.
  expect(el.type).toBe('button')
  expect(el.dataset['variant']).toBe('secondary')
  expect(el.dataset['block']).toBe('false')
})

test('type is overridable', () => {
  render(<Button type="submit">save</Button>, host)
  expect(host.querySelector('button')!.type).toBe('submit')
})

test('the affordance is a box, not brackets (vendor fork)', () => {
  render(<Button>ok</Button>, host)
  const sheet = host.querySelector('style')!.textContent!
  // Upstream draws `[ Label ]` with generated content; this fork keeps the
  // boxed presentation the app shipped with.
  expect(sheet).not.toContain('content:"[ "')
  expect(sheet).toContain('padding:0 1ch;')
  // The edge is an outline declared transparent, inset so ancestor clipping
  // cannot eat the focus ring; states only ever recolour it.
  expect(sheet).toContain('outline:var(--ms-border-width) solid transparent;')
  expect(sheet).toContain('outline-offset:-1px;')
  expect(sheet).toContain('text-transform:lowercase;')
  // No UA border, no radius: the box is padding + outline + fill.
  expect(sheet).toContain('border:0;')
  expect(sheet).not.toContain('border-radius')
})

test('variants select through data attributes, not per-instance CSS', () => {
  render(
    [
      Button({ variant: 'primary', children: 'a' }),
      Button({ variant: 'danger', children: 'b' }),
      Button({ variant: 'ghost', children: 'c' }),
    ],
    host,
  )
  const [primary, danger, ghost] = [...host.querySelectorAll('button')]
  expect(primary!.dataset['variant']).toBe('primary')
  expect(danger!.dataset['variant']).toBe('danger')
  expect(ghost!.dataset['variant']).toBe('ghost')

  // Three instances, one compiled stylesheet — the whole point of hoisting.
  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent)
  expect(sheets).toHaveLength(3)
  expect(new Set(sheets).size).toBe(1)

  const sheet = sheets[0]!
  expect(sheet).toContain(
    '[data-variant="primary"]{background:var(--accent);color:var(--fg-on-accent)',
  )
  expect(sheet).toContain('[data-variant="secondary"]{background:var(--bg-raised)')
  expect(sheet).toContain('[data-variant="danger"]{background:var(--danger)')
  // Ghost has no block of its own at rest: base colour, no fill.
  expect(sheet).not.toContain('[data-variant="ghost"]{color')
  // Hover steps the fill, not an underline.
  expect(sheet).toContain(
    '[data-variant="secondary"]:hover:not(:disabled){background:var(--border-strong)',
  )
  expect(sheet).toContain(
    '[data-variant="ghost"]:hover:not(:disabled){background:var(--bg-raised)',
  )
  expect(sheet).not.toContain('text-decoration:underline')
})

test('block snaps the width to whole cells', () => {
  render(<Button block>deploy</Button>, host)
  const el = host.querySelector('button')!
  expect(el.dataset['block']).toBe('true')
  const sheet = el.querySelector('style')!.textContent!
  expect(sheet).toContain('[data-block="true"]{display:flex;justify-content:center;width:100%;}')
  expect(sheet).toContain('round(down, 100%, 1ch)')
})

test('focus recolours the existing edge, and primary swaps ring colour', () => {
  render(<Button>deploy</Button>, host)
  const sheet = host.querySelector('style')!.textContent!
  const focus = sheet.slice(sheet.indexOf(':focus-visible{'))
  // One outline to go around: focus recolours it rather than drawing a second
  // ring, and there is no inversion (vendor fork of upstream's behaviour).
  expect(focus).toContain('outline-color:var(--accent)')
  expect(focus).toContain('[data-variant="primary"]:focus-visible{outline-color:var(--fg)')
  expect(focus).not.toContain('background:var(--fg)')
})

test('the state rules come after the variant rules they have to beat', () => {
  render(<Button>deploy</Button>, host)
  const sheet = host.querySelector('style')!.textContent!
  // Equal specificity (0,2,0), so source order is the only tiebreak.
  expect(sheet.indexOf('[data-variant="primary"]')).toBeLessThan(sheet.indexOf(':focus-visible'))
  expect(sheet.indexOf(':focus-visible')).toBeLessThan(sheet.indexOf(':scope:disabled'))
})

test('a click handler fires', () => {
  let clicks = 0
  render(<Button onclick={() => (clicks += 1)}>deploy</Button>, host)
  host.querySelector('button')!.click()
  flushSync()
  expect(clicks).toBe(1)
})

test('disabled reaches the element and is styled without colour alone', () => {
  let clicks = 0
  render(
    <Button disabled onclick={() => (clicks += 1)}>
      deploy
    </Button>,
    host,
  )
  const el = host.querySelector('button')!
  expect(el.disabled).toBe(true)

  el.click()
  flushSync()
  expect(clicks).toBe(0)

  const sheet = el.querySelector('style')!.textContent!
  expect(sheet).toContain(':disabled{color:var(--fg-subtle)')
  expect(sheet).toContain('cursor:not-allowed')
  // `pointer-events: none` would suppress the cursor the rule just asked for.
  expect(sheet).not.toContain('pointer-events')
  // Boxed variants keep a (dimmed) box; ghost has none to grey out.
  expect(sheet).toContain(
    ':disabled:not([data-variant="ghost"]){background:var(--bg-raised)',
  )
})

test('unknown props pass through to the element', () => {
  render(
    <Button attrs={{ 'aria-label': 'deploy to production' }} id="deploy">
      deploy
    </Button>,
    host,
  )
  const el = host.querySelector('button')!
  expect(el.id).toBe('deploy')
  expect(el.getAttribute('aria-label')).toBe('deploy to production')
})
