import { test, expect, beforeEach } from 'bun:test'
import { component, render, signal, tags, flushSync } from '../index.ts'

const { div, span } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

// ---------------------------------------------------------------------------
// Skipping re-renders on unchanged props
// ---------------------------------------------------------------------------

test('a child is not resumed when its props are shallow-equal', () => {
  let resumes = 0
  const Child = component<{ label: string }>(function* (props) {
    while (true) {
      resumes++
      yield span(props.label)
    }
  })

  const tick = signal(0)
  const App = component(function* () {
    yield () => {
      // `tick` is read so the parent re-renders, but the child's props do not
      // depend on it.
      return div(String(tick.value), Child({ label: 'stable' }))
    }
  })

  render(App(), host)
  expect(resumes).toBe(1)

  tick.value = 1
  flushSync()
  tick.value = 2
  flushSync()

  expect(resumes).toBe(1)
  expect(host.textContent).toBe('2stable')
})

test('a child is resumed when any prop value changes', () => {
  let resumes = 0
  const Child = component<{ label: string }>(function* (props) {
    while (true) {
      resumes++
      yield span(props.label)
    }
  })

  const label = signal('a')
  const App = component(function* () {
    yield () => div(Child({ label: label.value }))
  })

  render(App(), host)
  expect(resumes).toBe(1)

  label.value = 'b'
  flushSync()
  expect(resumes).toBe(2)
  expect(host.textContent).toBe('b')
})

test('adding or removing a prop counts as a change', () => {
  let resumes = 0
  const Child = component<{ a?: number; b?: number }>(function* (props) {
    while (true) {
      resumes++
      yield span(`${props.a ?? '-'}/${props.b ?? '-'}`)
    }
  })

  const extra = signal(false)
  const App = component(function* () {
    yield () =>
      div(extra.value ? Child({ a: 1, b: 2 }) : Child({ a: 1 }))
  })

  render(App(), host)
  expect(host.textContent).toBe('1/-')

  extra.value = true
  flushSync()
  expect(resumes).toBe(2)
  expect(host.textContent).toBe('1/2')

  extra.value = false
  flushSync()
  expect(resumes).toBe(3)
  expect(host.textContent).toBe('1/-')
})

test('memo: false always resumes', () => {
  let resumes = 0
  // A component reading mutable state its props do not describe. This is the
  // case the opt-out exists for.
  let external = 'first'
  const Child = component<{ label: string }>(
    function* (props) {
      while (true) {
        resumes++
        yield span(`${props.label}:${external}`)
      }
    },
    { memo: false },
  )

  const tick = signal(0)
  const App = component(function* () {
    yield () => div(String(tick.value), Child({ label: 'x' }))
  })

  render(App(), host)
  expect(host.textContent).toBe('0x:first')

  external = 'second'
  tick.value = 1
  flushSync()

  expect(resumes).toBe(2)
  expect(host.textContent).toBe('1x:second')
})

test('a signal read inside a memoized child still wakes it', () => {
  // Memoization only gates the parent-driven path. Anything the generator read
  // reaches it through the scheduler regardless.
  const inner = signal('one')
  const Child = component<{ label: string }>(function* (props) {
    yield () => span(`${props.label}-${inner.value}`)
  })

  const tick = signal(0)
  const App = component(function* () {
    yield () => div(String(tick.value), Child({ label: 'c' }))
  })

  render(App(), host)
  expect(host.textContent).toBe('0c-one')

  inner.value = 'two'
  flushSync()
  expect(host.textContent).toBe('0c-two')
})

// ---------------------------------------------------------------------------
// Yielding the same descriptor twice
// ---------------------------------------------------------------------------

test('re-yielding an identical descriptor skips the subtree', () => {
  let childResumes = 0
  const Child = component<{ n: number }>(function* (props) {
    while (true) {
      childResumes++
      yield span(String(props.n))
    }
  })

  const tick = signal(0)
  // Built once, in generator scope, and yielded unchanged on every resume.
  const frozen = div({ class: 'frozen' }, Child({ n: 1 }))

  const App = component(function* () {
    yield () => div(String(tick.value), frozen)
  })

  render(App(), host)
  expect(childResumes).toBe(1)

  tick.value = 1
  flushSync()
  expect(childResumes).toBe(1)
  expect(host.textContent).toBe('11')
  expect(host.querySelector('.frozen')).not.toBe(null)
})
