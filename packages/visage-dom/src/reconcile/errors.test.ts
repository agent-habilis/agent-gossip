import { test, expect, beforeEach } from 'bun:test'
import { component, disposable, flushSync, render, signal, tags } from '../index.ts'

const { div, span, ul, li } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

// ---------------------------------------------------------------------------
// A try/catch around the yield is the boundary
// ---------------------------------------------------------------------------

test('an ancestor catches a child that throws on mount', () => {
  const Boom = component(function* () {
    throw new Error('boom')
    // eslint-disable-next-line no-unreachable
    yield () => span('never')
  })

  const Boundary = component(function* () {
    try {
      yield () => div({ id: 'ok' }, Boom())
    } catch (error) {
      yield () => div({ id: 'failed' }, String((error as Error).message))
    }
  })

  render(Boundary(), host)
  expect(host.textContent).toBe('boom')
  expect(host.querySelector('#failed')).not.toBe(null)
  expect(host.querySelector('#ok')).toBe(null)
})

test('an ancestor catches a child that throws on a later re-render', () => {
  const explode = signal(false)

  const Child = component(function* () {
    yield () => {
      if (explode.value) throw new Error('late')
      return span('fine')
    }
  })

  const Boundary = component(function* () {
    try {
      yield () => div(Child())
    } catch (error) {
      yield () => div('caught ' + String((error as Error).message))
    }
  })

  render(Boundary(), host)
  expect(host.textContent).toBe('fine')

  explode.value = true
  flushSync()
  expect(host.textContent).toBe('caught late')
})

test('the nearest boundary wins', () => {
  const Boom = component(function* () {
    throw new Error('x')
    // eslint-disable-next-line no-unreachable
    yield () => span('never')
  })

  const Inner = component(function* () {
    try {
      yield () => div(Boom())
    } catch {
      yield () => div('inner caught')
    }
  })

  const Outer = component(function* () {
    try {
      yield () => div(Inner())
    } catch {
      yield () => div('outer caught')
    }
  })

  render(Outer(), host)
  expect(host.textContent).toBe('inner caught')
})

test('a boundary that rethrows passes the error along', () => {
  const Boom = component(function* () {
    throw new Error('original')
    // eslint-disable-next-line no-unreachable
    yield () => span('never')
  })

  const Middle = component(function* () {
    try {
      yield () => div(Boom())
    } catch (error) {
      // Wrap and rethrow: the generator ends here and the walk continues.
      throw new Error('wrapped: ' + String((error as Error).message))
    }
  })

  const Outer = component(function* () {
    try {
      yield () => div(Middle())
    } catch (error) {
      yield () => div(String((error as Error).message))
    }
  })

  render(Outer(), host)
  expect(host.textContent).toBe('wrapped: original')
})

// ---------------------------------------------------------------------------
// Unwinding
// ---------------------------------------------------------------------------

test('finally blocks run for every generator the error passes through', () => {
  const unwound: string[] = []

  const Boom = component(function* () {
    try {
      throw new Error('boom')
      // eslint-disable-next-line no-unreachable
      yield () => span('never')
    } finally {
      unwound.push('boom')
    }
  })

  const PassThrough = component(function* () {
    try {
      // No catch, so the error keeps going up and this generator is finished.
      yield () => div(Boom())
    } finally {
      unwound.push('passthrough')
    }
  })

  const Boundary = component(function* () {
    try {
      yield () => div(PassThrough())
    } catch {
      yield () => div('caught')
    }
  })

  render(Boundary(), host)
  expect(host.textContent).toBe('caught')
  expect(unwound).toEqual(['boom', 'passthrough'])
})

test('cleanups still run when the failed subtree is replaced', () => {
  const cleaned: string[] = []

  const Leaf = component(function* () {
    using _cleanup = disposable(() => cleaned.push('leaf'))
    yield () => span('leaf')
  })

  const explode = signal(false)
  const Child = component(function* () {
    yield () => {
      if (explode.value) throw new Error('boom')
      return div(Leaf())
    }
  })

  const Boundary = component(function* () {
    try {
      yield () => div(Child())
    } catch {
      yield () => div('caught')
    }
  })

  render(Boundary(), host)
  expect(cleaned).toEqual([])

  explode.value = true
  flushSync()
  expect(host.textContent).toBe('caught')
  expect(cleaned).toEqual(['leaf'])
})

// ---------------------------------------------------------------------------
// Nothing catches
// ---------------------------------------------------------------------------

test('render throws when no ancestor catches', () => {
  const Boom = component(function* () {
    throw new Error('unhandled')
    // eslint-disable-next-line no-unreachable
    yield () => span('never')
  })

  const App = component(function* () {
    yield () => div(Boom())
  })

  expect(() => render(App(), host)).toThrow(/unhandled/)
})

test('a boundary can retry, because the loop just comes back around', () => {
  const explode = signal(true)
  const retry = signal(0)

  const Child = component(function* () {
    while (true) {
      if (explode.value) throw new Error('boom')
      yield span('recovered')
    }
  })

  const Boundary = component(function* () {
    while (true) {
      try {
        yield div(Child())
      } catch {
        // Reading `retry` here is what makes the fallback live: it becomes a
        // dependency of *this* yield, so bumping it resumes the generator,
        // which falls out of the catch and re-enters the try.
        yield div(`caught, attempt ${retry.value}`)
      }
    }
  })

  render(Boundary(), host)
  expect(host.textContent).toBe('caught, attempt 0')

  // Retry while still broken: back to the fallback, with the new attempt.
  retry.value = 1
  flushSync()
  expect(host.textContent).toBe('caught, attempt 1')

  // Fix the cause, then retry: the try block succeeds this time.
  explode.value = false
  retry.value = 2
  flushSync()
  expect(host.textContent).toBe('recovered')
})

test('a fallback that reads nothing has no dependencies and stays put', () => {
  // Not a bug: everything read between resume and yield is a dependency of that
  // yield, and a static fallback reads nothing. Worth pinning so the retry
  // pattern above is understood as necessary rather than incidental.
  const explode = signal(false)
  const other = signal(0)

  const Child = component(function* () {
    yield () => {
      if (explode.value) throw new Error('boom')
      return span('fine')
    }
  })

  const Boundary = component(function* () {
    try {
      yield () => div(String(other.value), Child())
    } catch {
      yield () => div('static fallback')
    }
  })

  render(Boundary(), host)
  explode.value = true
  flushSync()
  expect(host.textContent).toBe('static fallback')

  other.value = 1
  flushSync()
  expect(host.textContent).toBe('static fallback')
})

// ---------------------------------------------------------------------------
// A failure inside a list must not corrupt its siblings
// ---------------------------------------------------------------------------

test('one bad row does not damage the rest of a keyed list', () => {
  const Row = component<{ n: number }>(function* (props) {
    yield () => {
      if (props.n === 3) throw new Error('bad row')
      return li(String(props.n))
    }
  })

  const Boundary = component(function* () {
    try {
      yield () => ul([1, 2, 3, 4].map((n) => Row({ key: n, n })))
    } catch (error) {
      yield () => div(String((error as Error).message))
    }
  })

  render(Boundary(), host)
  // The boundary replaces the whole list, which is the documented semantic:
  // the failing subtree is the one it yielded.
  expect(host.textContent).toBe('bad row')
})

// ---------------------------------------------------------------------------
// The scheduler must not drop a batch
// ---------------------------------------------------------------------------

test('a throwing subscriber does not skip the rest of the batch', () => {
  const tick = signal(0)
  const ran: string[] = []

  // No boundary above it, so its failure escapes to the scheduler.
  const Bad = component(function* () {
    yield () => {
      if (tick.value > 0) throw new Error('bad')
      ran.push('bad')
      return span('bad')
    }
  })

  const Good = component(function* () {
    yield () => {
      ran.push('good ' + String(tick.value))
      return span('good')
    }
  })

  const App = component(function* () {
    // Both children read `tick`, so both are queued in the same flush.
    yield () => div(Bad(), Good())
  })

  render(App(), host)
  ran.length = 0

  tick.value = 1
  flushSync()

  // `Good` ran even though `Bad` threw in the same batch.
  expect(ran).toContain('good 1')
})

// ---------------------------------------------------------------------------
// A boundary's own recovery commit must not be offered back to itself
// ---------------------------------------------------------------------------

test('a retrying boundary whose fallback throws on commit is bounded', () => {
  let attempts = 0
  const bad = signal(false)

  // The retry shape the dev warning in `handleError` recommends. The throw
  // comes from `mount` rather than from the generator, so it arrives through
  // `#commit` -> `queueFailure(this)` -> `leaveReconcile` -> `escalate`, which
  // used to hand it straight back to this same instance forever.
  const Boundary = component(function* () {
    while (true) {
      try {
        yield () =>
          bad.value
            ? div(
                span({
                  ref: () => {
                    attempts++
                    throw new Error('persistent')
                  },
                }),
              )
            : div('healthy')
      } catch {
        // Retry.
      }
    }
  })

  render(Boundary(), host)

  bad.value = true
  // `flush` reports per-subscriber errors rather than letting one failure drop
  // the batch, so nothing escapes here — the bug was never about the error
  // getting out, it was about never getting *back*.
  flushSync()

  // Bounded, rather than ~6800 attempts ending in a RangeError.
  expect(attempts).toBeLessThan(10)
})

// ---------------------------------------------------------------------------
// A failed mount unwinds what it already built
// ---------------------------------------------------------------------------

/** Records disposal through a real `using` scope, the only teardown there is. */
function tracked(log: string[], label: string): Disposable {
  return { [Symbol.dispose]: () => log.push(label) }
}

test('a ref throwing on the initial mount is catchable, and disposes the subtree', () => {
  const disposed: string[] = []
  const mounted: string[] = []

  const Leaf = component<{ label: string }>(function* (props) {
    const own = props.label
    mounted.push(own)
    using _tracked = tracked(disposed, own)
    while (true) yield () => span(props.label)
  })

  const Mid = component(function* () {
    while (true) {
      yield () =>
        div({ ref: () => { throw new Error('ref boom') } }, Leaf({ label: 'leaf' }))
    }
  })

  const Boundary = component(function* () {
    try {
      yield () => div(Mid())
    } catch (error) {
      yield () => div({ id: 'failed' }, String((error as Error).message))
    }
  })

  // Used to escape every boundary, because `Instance.mount` mounted the view
  // outside the try that routes to `#fail`.
  render(Boundary(), host)

  expect(host.querySelector('#failed')?.textContent).toBe('ref boom')
  expect(mounted).toEqual(['leaf'])
  // The leaf's fiber was never returned, so only `mount` itself could unwind it.
  expect(disposed).toEqual(['leaf'])
})

test('a ref throwing on a later patch disposes what that patch mounted', () => {
  const disposed: string[] = []
  const boom = signal(false)

  const Leaf = component<{ label: string }>(function* (props) {
    const own = props.label
    using _tracked = tracked(disposed, own)
    while (true) yield () => span(props.label)
  })

  const Boundary = component(function* () {
    while (true) {
      try {
        // Different tags, so the healthy node is replaced rather than patched.
        // `ref` only fires on mount, so reusing the element would never throw.
        yield () =>
          boom.value
            ? div({ ref: () => { throw new Error('late boom') } }, Leaf({ label: 'late' }))
            : span('healthy')
      } catch {
        yield () => div({ id: 'failed' }, 'failed')
      }
    }
  })

  render(Boundary(), host)
  boom.value = true
  flushSync()

  expect(host.querySelector('#failed')).not.toBeNull()
  expect(disposed).toEqual(['late'])
})

test('the DEV guard for a function in a child position does not leak siblings', () => {
  const disposed: string[] = []

  const Leaf = component(function* () {
    using _tracked = tracked(disposed, 'leaf')
    while (true) yield () => span('leaf')
  })

  const row = (): unknown => span('row')

  const Boundary = component(function* () {
    try {
      // `row` rather than `row()` — the typo the DEV guard exists to catch.
      yield () => div(Leaf(), row as never)
    } catch {
      yield () => div({ id: 'failed' }, 'failed')
    }
  })

  render(Boundary(), host)

  expect(host.querySelector('#failed')).not.toBeNull()
  expect(disposed).toEqual(['leaf'])
})

test('a mid-patch throw leaves no orphaned nodes behind', () => {
  const bad = signal(false)

  const Boundary = component(function* () {
    try {
      while (true) {
        yield () =>
          bad.value
            ? div('A', span({ ref: () => { throw new Error('boom') } }))
            : div('healthy')
      }
    } catch {
      yield () => div('FALLBACK')
    }
  })

  render(Boundary(), host)
  expect(host.textContent).toBe('healthy')

  bad.value = true
  flushSync()

  // Used to be "FALLBACK<span></span>" — the span was inserted by the failed
  // patch, referenced by no fiber, and therefore unremovable forever.
  expect(host.textContent).toBe('FALLBACK')
  expect(host.querySelector('span')).toBeNull()
})

test('the tree still renders after a failed patch', () => {
  const bad = signal(false)
  const label = signal('healthy')

  const retry = signal(0)

  // Plain yields rather than thunks: a yielded thunk parks the generator, so
  // bumping a signal re-calls the thunk instead of coming back around the loop.
  const App = component(function* () {
    while (true) {
      try {
        yield bad.value
          ? div(span('one'), span({ ref: () => { throw new Error('boom') } }))
          : div(label.value)
      } catch {
        // Reading `retry` makes the fallback live, so bumping it resumes the
        // generator, which falls out of the catch and re-enters the try.
        yield div('caught ' + String(retry.value))
      }
    }
  })

  render(App(), host)

  bad.value = true
  flushSync()
  expect(host.textContent).toBe('caught 0')

  // The real damage of a half-applied patch is not what it leaves on screen but
  // that the fiber tree now points at detached nodes, so every later render
  // writes into nothing. Recovering and re-rendering is what proves it does not.
  bad.value = false
  label.value = 'recovered'
  retry.value = 1
  flushSync()

  expect(host.textContent).toBe('recovered')
  expect(host.querySelectorAll('span').length).toBe(0)
})
