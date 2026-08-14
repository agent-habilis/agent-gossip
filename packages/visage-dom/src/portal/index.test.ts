import { test, expect, beforeEach } from 'bun:test'
import { component, disposable, flushSync, portal, render, signal, tags } from '../index.ts'

const { div, span, p, ul, li } = tags

let host: HTMLElement
let target: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  target = document.createElement('div')
  target.id = 'target'
  document.body.appendChild(host)
  document.body.appendChild(target)
})

/** Target contents with the portal's end markers stripped. */
const inTarget = () => target.innerHTML.replace(/<!---->/g, '')

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

test('children render into the target, not the host', () => {
  const App = component(function* () {
    yield () => div(p('local'), portal(target, span('remote')))
  })

  render(App(), host)
  expect(host.textContent).toBe('local')
  expect(inTarget()).toBe('<span>remote</span>')
})

test('the portal holds its place among logical siblings', () => {
  const App = component(function* () {
    yield () => div(p('a'), portal(target, span('x')), p('b'))
  })

  render(App(), host)
  // The placeholder sits between the two paragraphs, so ordering is preserved
  // for anything mounted after it.
  expect(host.textContent).toBe('ab')
  expect(host.querySelector('div')!.childNodes.length).toBe(3)
})

test('two portals share a target without interleaving', () => {
  const App = component(function* () {
    yield () =>
      div(
        portal(target, span('a1'), span('a2')),
        portal(target, span('b1'), span('b2')),
      )
  })

  render(App(), host)
  expect(target.textContent).toBe('a1a2b1b2')
})

// ---------------------------------------------------------------------------
// Updating
// ---------------------------------------------------------------------------

test('portal children update in place', () => {
  const label = signal('one')
  const App = component(function* () {
    yield () => div(portal(target, span(label.value)))
  })

  render(App(), host)
  expect(inTarget()).toBe('<span>one</span>')

  label.value = 'two'
  flushSync()
  expect(inTarget()).toBe('<span>two</span>')
})

test('a keyed list inside a portal reorders correctly', () => {
  const rows = signal([1, 2, 3])
  const App = component(function* () {
    yield () =>
      div(portal(target, ul(rows.value.map((n) => li({ key: n }, String(n))))))
  })

  render(App(), host)
  expect(target.textContent).toBe('123')

  rows.value = [3, 1, 2]
  flushSync()
  expect(target.textContent).toBe('312')

  rows.value = [2]
  flushSync()
  expect(target.textContent).toBe('2')
})

test('adding and removing portal children', () => {
  const items = signal<string[]>(['a'])
  const App = component(function* () {
    yield () => div(portal(target, ...items.value.map((s) => span(s))))
  })

  render(App(), host)
  expect(target.textContent).toBe('a')

  items.value = ['a', 'b', 'c']
  flushSync()
  expect(target.textContent).toBe('abc')

  items.value = []
  flushSync()
  expect(target.textContent).toBe('')
})

test('changing target moves the content', () => {
  const second = document.createElement('div')
  document.body.appendChild(second)

  const useSecond = signal(false)
  const App = component(function* () {
    yield () =>
      div(portal(useSecond.value ? second : target, span('moving')))
  })

  render(App(), host)
  expect(target.textContent).toBe('moving')
  expect(second.textContent).toBe('')

  useSecond.value = true
  flushSync()
  expect(target.textContent).toBe('')
  expect(second.textContent).toBe('moving')
})

// ---------------------------------------------------------------------------
// Teardown
// ---------------------------------------------------------------------------

test('unmounting the portal leaves the target clean', () => {
  const shown = signal(true)
  const App = component(function* () {
    yield () => div(shown.value ? portal(target, span('modal')) : p('closed'))
  })

  render(App(), host)
  expect(target.textContent).toBe('modal')

  shown.value = false
  flushSync()
  expect(host.textContent).toBe('closed')
  expect(target.childNodes.length).toBe(0)
})

test('components inside a portal are disposed on unmount', () => {
  const cleaned: string[] = []
  const Inner = component(function* () {
    using _cleanup = disposable(() => cleaned.push('inner'))
    yield () => span('inner')
  })

  const shown = signal(true)
  const App = component(function* () {
    yield () => div(shown.value ? portal(target, Inner()) : p('gone'))
  })

  render(App(), host)
  expect(target.textContent).toBe('inner')

  shown.value = false
  flushSync()
  expect(cleaned).toEqual(['inner'])
  expect(target.childNodes.length).toBe(0)
})

test('a portal nested inside a component that unmounts is cleaned up', () => {
  // The bulk-clear path removes the host subtree in one write, which can never
  // reach the target — so the portal has to clean itself during disposal.
  const Modal = component(function* () {
    yield () => div(portal(target, span('deep')))
  })

  const shown = signal(true)
  const App = component(function* () {
    yield () => div(shown.value ? Modal() : p('closed'))
  })

  render(App(), host)
  expect(target.textContent).toBe('deep')

  shown.value = false
  flushSync()
  expect(target.childNodes.length).toBe(0)
})

test('unmounting the whole root cleans the target', () => {
  const App = component(function* () {
    yield () => div(portal(target, span('x')))
  })

  const root = render(App(), host)
  expect(target.textContent).toBe('x')

  root.unmount()
  expect(target.childNodes.length).toBe(0)
})

test('a portal inside a keyed list moves without disturbing its content', () => {
  const rows = signal([1, 2])
  const Row = component<{ n: number }>(function* (props) {
    yield () =>
      li(String(props.n), portal(target, span(`p${props.n}`)))
  })
  const App = component(function* () {
    yield () => ul(rows.value.map((n) => Row({ key: n, n })))
  })

  render(App(), host)
  expect(host.textContent).toBe('12')
  expect(target.textContent).toBe('p1p2')

  rows.value = [2, 1]
  flushSync()
  // The logical order flips; portal content keeps its own target order, which
  // is by mount time rather than by sibling position.
  expect(host.textContent).toBe('21')
  expect(target.textContent).toBe('p1p2')

  rows.value = [2]
  flushSync()
  expect(host.textContent).toBe('2')
  expect(target.textContent).toBe('p2')
})
