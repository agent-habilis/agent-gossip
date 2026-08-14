import { beforeEach, expect, test } from 'bun:test'
import { render } from 'visage-dom'
import {
  Badge,
  Box,
  Button,
  Checkbox,
  Divider,
  Input,
  Kbd,
  MiddleTruncate,
  Note,
  ProgressBar,
  Radio,
  Select,
  Spinner,
  Stack,
  StatusDot,
  Table,
  Tabs,
  Text,
} from './index.ts'

/*
 * A sweep over every component's compiled CSS, for the one failure mode that is
 * invisible everywhere else.
 *
 * visage-style appends `px` to a bare number unless the property is in its
 * unitless set — and the set is narrower than CSS's. `fontWeight: 400` compiles
 * to `font-weight:400px` and `flex: 1` to `flex:1px`; both typecheck, both
 * render, and both are dropped by the browser. Nothing in a DOM-structure test
 * or a visual glance catches it, because the declaration simply is not there.
 *
 * This ran green only after fixing a real instance of it in `Text`, so it is a
 * regression test, not a hypothetical one.
 */

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/** Every component, mounted with just enough props to render. */
const EVERY_COMPONENT = (): unknown[] => [
  Text({ children: 'x', weight: 'bold' }),
  Box({ title: 'x', children: 'y' }),
  Stack({ gap: 1, children: 'x' }),
  Divider({}),
  Button({ variant: 'primary', children: 'x' }),
  Input({ value: 'x' }),
  Select({ options: [{ value: 'a', label: 'A' }] }),
  Checkbox({ children: 'x' }),
  Radio({ name: 'g', value: 'a', children: 'x' }),
  Badge({ children: 'x' }),
  StatusDot({ status: 'ready', children: 'x' }),
  Table({
    columns: [{ key: 'name', header: 'Name' }],
    rows: [{ name: 'a' }],
    rowKey: (row: { name: string }) => row.name,
  }),
  ProgressBar({ value: 50, label: 'Uploading' }),
  Spinner({}),
  MiddleTruncate({ value: 'a/b/c', budget: 3 }),
  Kbd({ children: 'x' }),
  Note({ children: 'x' }),
  Tabs({ tabs: [{ id: 'a', label: 'A', content: 'x' }] }),
]

/**
 * Properties CSS treats as unitless, which a stray `px` therefore destroys.
 *
 * `lineHeight` is deliberately absent: a length there is legal, and the system
 * uses `calc(1 * var(--ms-row))` on purpose.
 */
const UNITLESS = [
  'font-weight',
  'flex',
  'flex-grow',
  'flex-shrink',
  'opacity',
  'z-index',
  'order',
  'scale',
]

test('no component emits a unitless property with a px suffix', () => {
  render(EVERY_COMPONENT() as never, host)

  const sheets = [...host.querySelectorAll('style')].map((s) => s.textContent ?? '')
  expect(sheets.length).toBeGreaterThan(10)

  const offenders: string[] = []
  for (const sheet of sheets) {
    for (const property of UNITLESS) {
      // `flex-grow` would also match a search for `flex`, so anchor on the colon.
      const match = sheet.match(new RegExp(`${property}:[^;]*\\d+px`))
      if (match) offenders.push(match[0])
    }
  }

  expect(offenders).toEqual([])
})

test('every component mounts and unmounts without leaving DOM behind', () => {
  /*
   * The unmount half is the valuable half. Spinner holds an `interval`, and
   * MiddleTruncate a `ResizeObserver`; an undisposed resource shows up here as
   * leftover DOM, and in the worst case as a test run that never exits.
   * Comment nodes are visage's position holes, not leaks.
   */
  const root = render(EVERY_COMPONENT() as never, host)
  expect(host.querySelectorAll('style').length).toBeGreaterThan(10)

  root.unmount()
  expect(host.innerHTML.replace(/<!---->/g, '')).toBe('')
})

test('no component inlines a raw hex — colour always travels as a token', () => {
  render(EVERY_COMPONENT() as never, host)

  const inlined = [...host.querySelectorAll<HTMLElement>('[style]')]
    .map((el) => el.getAttribute('style') ?? '')
    .filter((style) => /#[0-9a-f]{3,8}\b/i.test(style))

  // A hex on an element means that component resolved a palette value itself
  // instead of pointing at a custom property, and would not follow a theme.
  expect(inlined).toEqual([])
})
