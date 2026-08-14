import { test, expect, beforeEach } from 'bun:test'
import { component, render, signal, tags, flushSync, untrack } from '../index.ts'

const { div, span, button } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

// ---------------------------------------------------------------------------
// Props are tracked reads: only the keys a component actually read matter.
// ---------------------------------------------------------------------------

test('a prop the component never reads does not resume it', () => {
  let resumes = 0
  const Child = component<{ shown: string; ignored: number }>(function* (props) {
    while (true) {
      resumes++
      yield span(props.shown)
    }
  })

  const ignored = signal(0)
  const App = component(function* () {
    yield () => div(Child({ shown: 'fixed', ignored: ignored.value }))
  })

  render(App(), host)
  expect(resumes).toBe(1)

  // The parent re-renders and hands the child a different `ignored` every time.
  // The child never reads it, so it is not a dependency of anything it yielded.
  ignored.value = 1
  flushSync()
  ignored.value = 2
  flushSync()

  expect(resumes).toBe(1)
  expect(host.textContent).toBe('fixed')
})

test('a prop the component does read resumes it', () => {
  let resumes = 0
  const Child = component<{ shown: string }>(function* (props) {
    while (true) {
      resumes++
      yield span(props.shown)
    }
  })

  const shown = signal('one')
  const App = component(function* () {
    yield () => div(Child({ shown: shown.value }))
  })

  render(App(), host)
  expect(resumes).toBe(1)

  shown.value = 'two'
  flushSync()

  expect(resumes).toBe(2)
  expect(host.textContent).toBe('two')
})

test('which props matter is recomputed every yield', () => {
  // The same dynamic-dependency property signals have: the set is rebuilt at
  // each yield, so switching branches switches which prop can wake it.
  const useA = signal(true)
  const a = signal('a1')
  const b = signal('b1')

  let resumes = 0
  const Child = component<{ a: string; b: string; pick: boolean }>(function* (props) {
    while (true) {
      resumes++
      yield span(props.pick ? props.a : props.b)
    }
  })

  const App = component(function* () {
    yield () => div(Child({ a: a.value, b: b.value, pick: useA.value }))
  })

  render(App(), host)
  expect(host.textContent).toBe('a1')
  const afterMount = resumes

  // Reading `a` was what the last yield did, so `b` changing is not its business.
  b.value = 'b2'
  flushSync()
  expect(resumes).toBe(afterMount)

  a.value = 'a2'
  flushSync()
  expect(host.textContent).toBe('a2')

  // Flip the branch; now `b` is the live one and `a` is not.
  useA.value = false
  flushSync()
  expect(host.textContent).toBe('b2')
  const afterFlip = resumes

  a.value = 'a3'
  flushSync()
  expect(resumes).toBe(afterFlip)

  b.value = 'b3'
  flushSync()
  expect(host.textContent).toBe('b3')
})

test('props read in a handler are current, not captured', () => {
  // A read outside the resume window records nothing and sees the value as of
  // the moment it runs — so a handler created once cannot close over a stale
  // prop, which is the bug `useCallback` dependency arrays exist to prevent.
  let seen = ''
  const Child = component<{ label: string }>(function* (props) {
    const onclick = (): void => {
      seen = props.label
    }
    yield () => button({ onclick }, 'go')
  })

  const label = signal('first')
  const App = component(function* () {
    yield () => div(Child({ label: label.value }))
  })

  render(App(), host)
  label.value = 'second'
  flushSync()

  host.querySelector('button')!.click()
  expect(seen).toBe('second')
})

test('untrack opts a prop read out of waking the component', () => {
  let resumes = 0
  const Child = component<{ seed: number }>(function* (props) {
    // The seeding idiom: read once on purpose, do not depend on it after.
    const initial = untrack(() => props.seed)
    while (true) {
      resumes++
      yield span(String(initial))
    }
  })

  const seed = signal(1)
  const App = component(function* () {
    yield () => div(Child({ seed: seed.value }))
  })

  render(App(), host)
  expect(host.textContent).toBe('1')

  seed.value = 2
  flushSync()

  expect(resumes).toBe(1)
  expect(host.textContent).toBe('1')
})

test('yield carries nothing back into the generator', () => {
  // Props are live, so there is nothing to hand back and the slot is empty.
  // Keeping it empty is what lets `P` be inferred from the parameter — see
  // spikes/01-yield-typing.ts for why anything in it breaks that.
  const sent: unknown[] = []
  const Child = component<{ label: string }>(function* (props) {
    while (true) {
      sent.push(yield span(props.label))
    }
  })

  const label = signal('one')
  const App = component(function* () {
    yield () => div(Child({ label: label.value }))
  })

  render(App(), host)
  label.value = 'two'
  flushSync()

  expect(host.textContent).toBe('two')
  expect(sent).toEqual([undefined])
})

test('spreading props depends on all of them', () => {
  let resumes = 0
  const Child = component<{ a: string; b: string }>(function* (props) {
    while (true) {
      resumes++
      const copy = { ...props }
      yield span(`${copy.a}${copy.b}`)
    }
  })

  const b = signal('1')
  const App = component(function* () {
    yield () => div(Child({ a: 'x', b: b.value }))
  })

  render(App(), host)
  b.value = '2'
  flushSync()

  expect(resumes).toBe(2)
  expect(host.textContent).toBe('x2')
})

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

test('destructuring props in the parameter list throws', () => {
  // Parameters bind when the generator function is called, before the body
  // runs, so this read is unambiguously parameter destructuring and the
  // component would render its mount-time values forever.
  const Bad = component<{ label: string }>(function* ({ label }: { label: string }) {
    yield () => span(label)
  })

  expect(() => render(div(Bad({ label: 'x' })), host)).toThrow(/destructured its props/)
})

test('assigning to a prop throws', () => {
  const Bad = component<{ label: string }>(function* (props) {
    yield () => {
      ;(props as { label: string }).label = 'nope'
      return span(props.label)
    }
  })

  expect(() => render(div(Bad({ label: 'x' })), host)).toThrow(/assigned to a prop/)
})

test('reading props after an await needs this.track, and then it works', async () => {
  // The synchronous window closes at the first await for props exactly as it
  // does for signals, and this.track reopens it for both.
  const { asyncComponent } = await import('../index.ts')

  let tracked = ''
  const Child = asyncComponent<{ label: string }>(async function* (props) {
    while (true) {
      await Promise.resolve()
      tracked = this.track(() => props.label)
      yield span(tracked)
      await new Promise((resolve) => setTimeout(resolve, 0))
    }
  })

  render(div(Child({ label: 'first' })), host)
  await new Promise((resolve) => setTimeout(resolve, 10))
  expect(tracked).toBe('first')
})
