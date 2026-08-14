import { test, expect, beforeEach } from 'bun:test'
import { component, disposable, flushSync, render, signal, tags } from '../index.ts'

const { div, span, ul, li, p } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

const html = () => host.innerHTML

// ---------------------------------------------------------------------------
// A component may yield an array
// ---------------------------------------------------------------------------

test('a component can yield an array as its root', () => {
  const Multi = component(function* () {
    yield () => [span('a'), span('b')]
  })
  const App = component(function* () {
    yield () => div(Multi())
  })

  render(App(), host)
  expect(html()).toBe('<div><span>a</span><span>b</span></div>')
})

test('render accepts an array root', () => {
  render([span('a'), span('b')], host)
  expect(html()).toBe('<span>a</span><span>b</span>')
})

test('an empty fragment holds its position', () => {
  const items = signal<string[]>([])
  const Multi = component(function* () {
    yield () => items.value.map((s) => span(s))
  })
  const App = component(function* () {
    yield () => div(p('before'), Multi(), p('after'))
  })

  render(App(), host)
  // One hole so the fragment always owns a node; siblings keep their order.
  expect(host.textContent).toBe('beforeafter')

  items.value = ['x', 'y']
  flushSync()
  expect(html()).toBe('<div><p>before</p><span>x</span><span>y</span><p>after</p></div>')

  items.value = []
  flushSync()
  expect(host.textContent).toBe('beforeafter')
  expect(host.querySelectorAll('span').length).toBe(0)
})

test('a fragment grows and shrinks between siblings', () => {
  const n = signal(1)
  const Multi = component(function* () {
    yield () => Array.from({ length: n.value }, (_, i) => span(String(i)))
  })
  const App = component(function* () {
    yield () => div(p('L'), Multi(), p('R'))
  })

  render(App(), host)
  expect(host.textContent).toBe('L0R')

  n.value = 3
  flushSync()
  expect(host.textContent).toBe('L012R')

  n.value = 2
  flushSync()
  expect(host.textContent).toBe('L01R')
})

test('nested fragments flatten into one run', () => {
  const Inner = component(function* () {
    yield () => [span('b'), span('c')]
  })
  const Outer = component(function* () {
    yield () => [span('a'), Inner(), span('d')]
  })
  const App = component(function* () {
    yield () => div(Outer())
  })

  render(App(), host)
  expect(host.textContent).toBe('abcd')
})

test('a component switching between an element root and a fragment root', () => {
  const many = signal(false)
  const Switch = component(function* () {
    yield () => many.value ? [span('a'), span('b')] : span('one')
  })
  const App = component(function* () {
    yield () => div(p('L'), Switch(), p('R'))
  })

  render(App(), host)
  expect(host.textContent).toBe('LoneR')

  many.value = true
  flushSync()
  expect(host.textContent).toBe('LabR')

  many.value = false
  flushSync()
  expect(host.textContent).toBe('LoneR')
})

// ---------------------------------------------------------------------------
// The hard part: multi-node fibers moving through the keyed ordering pass
// ---------------------------------------------------------------------------

/** Each row is a component whose root is a fragment, so it owns two nodes. */
const Pair = component<{ n: number }>(function* (props) {
  yield () =>
    [li(`${props.n}a`), li(`${props.n}b`)]
})

const order = () => [...host.querySelectorAll('li')].map((el) => el.textContent).join(' ')

test('fragments in a keyed list reorder as whole units', () => {
  const rows = signal([1, 2, 3])
  const App = component(function* () {
    yield () => ul(rows.value.map((n) => Pair({ key: n, n })))
  })

  render(App(), host)
  expect(order()).toBe('1a 1b 2a 2b 3a 3b')

  rows.value = [3, 1, 2]
  flushSync()
  expect(order()).toBe('3a 3b 1a 1b 2a 2b')

  rows.value = [2, 3, 1]
  flushSync()
  expect(order()).toBe('2a 2b 3a 3b 1a 1b')

  rows.value = [1, 2, 3].reverse()
  flushSync()
  expect(order()).toBe('3a 3b 2a 2b 1a 1b')
})

test('a keyed fragment row keeps its DOM nodes across a move', () => {
  const rows = signal([1, 2, 3])
  const App = component(function* () {
    yield () => ul(rows.value.map((n) => Pair({ key: n, n })))
  })

  render(App(), host)
  const before = [...host.querySelectorAll('li')]
  const firstOfRowOne = before[0]!
  const secondOfRowOne = before[1]!

  rows.value = [2, 3, 1]
  flushSync()

  const after = [...host.querySelectorAll('li')]
  // Moved, not rebuilt: same nodes, now at the end, still adjacent and ordered.
  expect(after[4] === firstOfRowOne).toBe(true)
  expect(after[5] === secondOfRowOne).toBe(true)
})

test('inserting and removing fragment rows keeps every row intact', () => {
  const rows = signal([1, 2])
  const App = component(function* () {
    yield () => ul(rows.value.map((n) => Pair({ key: n, n })))
  })

  render(App(), host)
  expect(order()).toBe('1a 1b 2a 2b')

  rows.value = [1, 3, 2]
  flushSync()
  expect(order()).toBe('1a 1b 3a 3b 2a 2b')

  rows.value = [3]
  flushSync()
  expect(order()).toBe('3a 3b')

  rows.value = []
  flushSync()
  expect(host.querySelectorAll('li').length).toBe(0)
})

test('a fragment row sits correctly beside single-node rows', () => {
  const Single = component<{ n: number }>(function* (props) {
    yield () => li(`s${props.n}`)
  })
  const rows = signal([1, 2, 3])
  const App = component(function* () {
    yield () =>
      ul(
        rows.value.map((n) => (n === 2 ? Pair({ key: n, n }) : Single({ key: n, n }))),
      )
  })

  render(App(), host)
  expect(order()).toBe('s1 2a 2b s3')

  rows.value = [3, 2, 1]
  flushSync()
  expect(order()).toBe('s3 2a 2b s1')

  rows.value = [2, 1, 3]
  flushSync()
  expect(order()).toBe('2a 2b s1 s3')
})

test('unmounting a fragment row removes all of its nodes', () => {
  const rows = signal([1, 2, 3])
  const App = component(function* () {
    yield () => ul(rows.value.map((n) => Pair({ key: n, n })))
  })

  render(App(), host)
  rows.value = [1, 3]
  flushSync()
  expect(order()).toBe('1a 1b 3a 3b')
  expect(host.querySelectorAll('li').length).toBe(4)
})

test('cleanups run when a fragment row is removed', () => {
  const cleaned: number[] = []
  const Tracked = component<{ n: number }>(function* (props) {
    const id = props.n
    using _cleanup = disposable(() => cleaned.push(id))
    yield () => [li(`${props.n}a`), li(`${props.n}b`)]
  })

  const rows = signal([1, 2])
  const App = component(function* () {
    yield () => ul(rows.value.map((n) => Tracked({ key: n, n })))
  })

  render(App(), host)
  rows.value = [2]
  flushSync()
  expect(cleaned).toEqual([1])

  rows.value = []
  flushSync()
  expect(cleaned).toEqual([1, 2])
  expect(host.querySelectorAll('li').length).toBe(0)
})
