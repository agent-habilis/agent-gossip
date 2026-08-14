import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import { Box } from './Box.tsx'

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/**
 * The rendered text, excluding every scoped `<style>` element's own contents.
 *
 * Recursive rather than one level deep, because a Box with a title nests a Text
 * and a Divider, each carrying a stylesheet of its own.
 */
const label = (el: Node): string =>
  [...el.childNodes]
    .filter((n) => n.nodeName !== 'STYLE')
    .map((n) => (n.childNodes.length > 0 ? label(n) : n.textContent))
    .join('')

/** The Box's own stylesheet — its first child, never a descendant's. */
const sheet = (el: Element): string => el.firstElementChild!.textContent!

test('renders a div, with the padding carried as custom properties', () => {
  render(<Box>content</Box>, host)
  const el = host.querySelector('div')!
  expect(label(el)).toBe('content')
  expect(el.style.getPropertyValue('--ms-box-pad-x')).toBe('1ch')
  expect(el.style.getPropertyValue('--ms-box-pad-y')).toBe('calc(0 * var(--ms-row))')
  expect(el.style.getPropertyValue('--ms-box-bg')).toBe('transparent')

  const css = sheet(el)
  expect(css).toContain('padding-block:var(--ms-box-pad-y)')
  expect(css).toContain('padding-inline:var(--ms-box-pad-x)')
})

test('padding is measured in cells across and rows down', () => {
  render(<Box padX={3} padY={2} />, host)
  const el = host.querySelector('div')!
  expect(el.style.getPropertyValue('--ms-box-pad-x')).toBe('3ch')
  expect(el.style.getPropertyValue('--ms-box-pad-y')).toBe('calc(2 * var(--ms-row))')
})

test('as changes the element without changing the styling', () => {
  render(<Box as="section">panel</Box>, host)
  const el = host.querySelector('section')!
  expect(el).not.toBeNull()
  expect(sheet(el)).toContain('padding-inline:var(--ms-box-pad-x)')
})

test('the border is an outline, so it costs no layout', () => {
  render(<Box border="line" borderColor="accent" />, host)
  const el = host.querySelector('div')!
  expect(el.dataset['border']).toBe('line')
  expect(el.style.getPropertyValue('--ms-box-border-color')).toBe('var(--accent)')

  const css = sheet(el)
  expect(css).toContain(
    ':scope[data-border="line"]{outline:1px solid var(--ms-box-border-color);}',
  )
  expect(css).toContain('outline-offset:0')
  // A `border` would make the box 2px taller than a whole number of rows.
  expect(css).not.toContain('border-width')
})

test('the three border weights are enumerated in one stylesheet', () => {
  render(<Box border="double" />, host)
  const css = sheet(host.querySelector('div')!)
  expect(css).toContain('outline:3px double var(--ms-box-border-color)')
  expect(css).toContain('outline:2px solid var(--ms-box-border-color)')
  expect(host.querySelector('div')!.dataset['border']).toBe('double')
})

test('a numeric width becomes a cell count in a custom property', () => {
  render(<Box width={20} />, host)
  const el = host.querySelector('div')!
  // The selector knows the kind; the number itself never reaches the CSS.
  expect(el.dataset['width']).toBe('fixed')
  expect(el.style.getPropertyValue('--ms-box-width')).toBe('20ch')
  expect(sheet(el)).toContain(':scope[data-width="fixed"]{width:var(--ms-box-width);}')
})

test('full snaps down to a whole cell, guarded by @supports', () => {
  render(<Box />, host)
  const el = host.querySelector('div')!
  expect(el.dataset['width']).toBe('full')
  expect(el.dataset['unsnapped']).toBe('false')

  const css = sheet(el)
  expect(css).toContain('@supports (width: round(down, 100%, 1ch))')
  expect(css).toContain('width:round(down, 100%, 1ch)')
})

test('measure caps at the 80-column measure and snaps within it', () => {
  render(<Box width="measure" />, host)
  const css = sheet(host.querySelector('div')!)
  expect(css).toContain(':scope[data-width="measure"]{')
  expect(css).toContain('max-width:var(--ms-measure)')
  expect(css).toContain('max-width:min(var(--ms-measure), round(down, 100%, 1ch))')
})

test('auto shrinks to content', () => {
  render(<Box width="auto">x</Box>, host)
  const el = host.querySelector('div')!
  expect(el.dataset['width']).toBe('auto')
  expect(sheet(el)).toContain(':scope[data-width="auto"]{width:max-content;}')
})

test('unsnapped opts out of round() while keeping the measure ceiling', () => {
  render(<Box width="measure" unsnapped />, host)
  const el = host.querySelector('div')!
  expect(el.dataset['unsnapped']).toBe('true')

  const css = sheet(el)
  // Equal specificity with [data-width="measure"], so it wins on source order.
  const override = css.indexOf(':scope[data-unsnapped="true"]{width:100%;}')
  expect(override).toBeGreaterThan(css.indexOf(':scope[data-width="measure"]{'))
  expect(css).toContain(
    ':scope[data-width="measure"][data-unsnapped="true"]{max-width:var(--ms-measure);}',
  )
})

test('a title renders a header and a rule ahead of the children', () => {
  render(<Box title="output" borderColor="accent">body</Box>, host)
  const el = host.querySelector('div')!
  expect(label(el)).toBe('outputbody')

  const header = el.querySelector('div[data-caps="true"]')!
  expect(label(header)).toBe('output')
  expect(header.getAttribute('style')).toContain('--ms-text-color: var(--fg-muted)')

  // The rule takes the box's own border colour, so a bordered panel matches.
  const divider = el.querySelector('[role="separator"]')! as HTMLElement
  expect(divider.style.getPropertyValue('--ms-divider-color')).toBe('var(--accent)')
  expect(header.compareDocumentPosition(divider) & Node.DOCUMENT_POSITION_FOLLOWING).
    toBeTruthy()
})

test('no title means no header row at all', () => {
  render(<Box>body</Box>, host)
  const el = host.querySelector('div')!
  expect(el.querySelector('[role="separator"]')).toBeNull()
  expect(label(el)).toBe('body')
})

test('two instances share one compiled stylesheet', () => {
  render([Box({ padX: 2, children: 'a' }), Box({ border: 'thick', children: 'b' })], host)
  const boxes = [...host.querySelectorAll(':scope > div')]
  expect(boxes).toHaveLength(2)

  const sheets = boxes.map((b) => sheet(b))
  expect(new Set(sheets).size).toBe(1)
})

test('a caller style prop survives alongside the box properties', () => {
  render(<Box style={{ opacity: '0.5' }} />, host)
  const el = host.querySelector('div')! as HTMLElement
  expect(el.style.opacity).toBe('0.5')
  expect(el.style.getPropertyValue('--ms-box-pad-x')).toBe('1ch')
})
