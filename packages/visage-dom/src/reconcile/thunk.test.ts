/**
 * Render thunks: `yield () => view` parks the generator.
 *
 * The generator is never resumed for an update again — it sits suspended at
 * that yield for the component's life, which is what keeps `using`,
 * `try`/`finally`, `yield*` teardown and `gen.throw()` working exactly as they
 * do for a looping component. Updates call the thunk instead.
 *
 * The loop form is unchanged and is exercised by every other test file here.
 */

import { test, expect, beforeEach, afterEach, spyOn } from 'bun:test'
import {
  component,
  asyncComponent,
  context,
  disposable,
  render,
  signal,
  tags,
  flushSync,
  untrack,
} from '../index.ts'
import type { Behavior, View } from '../index.ts'

const { div, span, ul, li } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

const html = () => host.innerHTML.replace(/<!---->/g, '')
const tick = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms))

// ---------------------------------------------------------------------------
// The feature: the generator runs once
// ---------------------------------------------------------------------------

test('the generator is resumed once, however many times the thunk renders', () => {
  let resumes = 0
  const n = signal(0)

  const Counter = component(function* () {
    resumes++
    yield () => div(String(n.value))
  })

  render(Counter(), host)
  expect(resumes).toBe(1)
  expect(html()).toBe('<div>0</div>')

  for (let i = 1; i <= 5; i++) {
    n.value = i
    flushSync()
  }

  expect(html()).toBe('<div>5</div>')
  // The whole point: five renders, one resume.
  expect(resumes).toBe(1)
})

test('setup runs once, and its closures keep their identity', () => {
  let setups = 0
  const n = signal(0)
  const handlers = new Set<unknown>()

  const C = component(function* () {
    setups++
    const inc = () => n.value++
    yield () => {
      handlers.add(inc)
      return div({ onclick: inc }, String(n.value))
    }
  })

  render(C(), host)
  n.value = 1
  flushSync()
  n.value = 2
  flushSync()

  expect(setups).toBe(1)
  expect(handlers.size).toBe(1)
})

test('a thunk with a multi-statement body, as a loop body becomes', () => {
  const items = signal<string[]>([])

  const List = component(function* () {
    yield () => {
      const current = items.value
      if (current.length === 0) return span('nothing yet')
      return ul(current.map((text) => li(text)))
    }
  })

  render(List(), host)
  expect(html()).toBe('<span>nothing yet</span>')

  items.value = ['a', 'b']
  flushSync()
  expect(html()).toBe('<ul><li>a</li><li>b</li></ul>')
})

test('every View member is a legal thunk return', () => {
  const which = signal(0)
  const C = component(function* () {
    yield (): View => {
      switch (which.value) {
        case 0:
          return null
        case 1:
          return 'text'
        case 2:
          return 42
        case 3:
          return [div('a'), div('b')]
        default:
          return div('el')
      }
    }
  })

  render(C(), host)
  expect(html()).toBe('')

  for (const [value, expected] of [
    [1, 'text'],
    [2, '42'],
    [3, '<div>a</div><div>b</div>'],
    [4, '<div>el</div>'],
  ] as const) {
    which.value = value
    flushSync()
    expect(html()).toBe(expected)
  }
})

// ---------------------------------------------------------------------------
// Tracking
// ---------------------------------------------------------------------------

test('dependencies are recomputed on every thunk call', () => {
  const toggle = signal(true)
  const a = signal('A')
  const b = signal('B')
  let renders = 0

  const C = component(function* () {
    yield () => {
      renders++
      return div(toggle.value ? a.value : b.value)
    }
  })

  render(C(), host)
  expect(html()).toBe('<div>A</div>')
  expect(renders).toBe(1)

  // `b` is not a dependency while `toggle` is true.
  b.value = 'B2'
  flushSync()
  expect(renders).toBe(1)

  a.value = 'A2'
  flushSync()
  expect(html()).toBe('<div>A2</div>')
  expect(renders).toBe(2)

  toggle.value = false
  flushSync()
  expect(html()).toBe('<div>B2</div>')

  // Now the other way round: `a` no longer wakes it, `b` does.
  a.value = 'A3'
  flushSync()
  expect(html()).toBe('<div>B2</div>')

  b.value = 'B3'
  flushSync()
  expect(html()).toBe('<div>B3</div>')
})

test('a signal read only during setup is not a dependency', () => {
  const seed = signal('first')
  let renders = 0

  const C = component(function* () {
    // Read above the yield: setup, not render. Deliberate one-time reads say so
    // with untrack(); this one does not, and gets the warning tested below.
    const captured = untrack(() => seed.value)
    yield () => {
      renders++
      return div(captured)
    }
  })

  render(C(), host)
  expect(html()).toBe('<div>first</div>')

  seed.value = 'second'
  flushSync()

  expect(renders).toBe(1)
  expect(html()).toBe('<div>first</div>')
})

test('a prop read in the thunk resumes it; an unread prop does not', () => {
  let renders = 0
  const Child = component<{ shown: string; ignored: number }>(function* (props) {
    yield () => {
      renders++
      return span(props.shown)
    }
  })

  const shown = signal('a')
  const ignored = signal(0)
  const App = component(function* () {
    yield () => div(Child({ shown: shown.value, ignored: ignored.value }))
  })

  render(App(), host)
  expect(renders).toBe(1)

  ignored.value = 1
  flushSync()
  expect(renders).toBe(1)

  shown.value = 'b'
  flushSync()
  expect(renders).toBe(2)
  expect(html()).toBe('<div><span>b</span></div>')
})

test('this.track inside a thunk survives the resubscribe', () => {
  const n = signal(0)
  let renders = 0

  const C = component(function* () {
    yield () => {
      renders++
      // Reading through this.track() must subscribe just as a bare read does.
      const value = this.track(() => n.value)
      return div(String(value))
    }
  })

  render(C(), host)
  expect(renders).toBe(1)

  n.value = 1
  flushSync()
  expect(renders).toBe(2)
  expect(html()).toBe('<div>1</div>')

  n.value = 2
  flushSync()
  expect(renders).toBe(3)
  expect(html()).toBe('<div>2</div>')
})

test('re-yielding an identical descriptor from a thunk skips the subtree', () => {
  let childRenders = 0
  const Child = component(function* () {
    childRenders++
    yield () => span('static')
  })

  const tick = signal(0)
  const hoisted = div(Child())

  const App = component(function* () {
    yield () => {
      void tick.value
      return hoisted
    }
  })

  render(App(), host)
  expect(childRenders).toBe(1)

  tick.value = 1
  flushSync()
  expect(childRenders).toBe(1)
})

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

test('finally and using run when a parked component unmounts', () => {
  const order: string[] = []

  const C = component(function* () {
    using _res = disposable(() => order.push('disposed'))
    try {
      yield () => div('x')
    } finally {
      order.push('finally')
    }
  })

  const mounted = signal(true)
  const App = component(function* () {
    yield () => (mounted.value ? div(C()) : div())
  })

  render(App(), host)
  expect(order).toEqual([])

  mounted.value = false
  flushSync()

  // Inner first, then outer — ordinary generator unwinding.
  expect(order).toEqual(['finally', 'disposed'])
})

test('a using scope unwinds when a parked component unmounts', () => {
  let cleaned = 0
  const C = component(function* () {
    using _cleanup = disposable(() => cleaned++)
    yield () => div('x')
  })

  const mounted = signal(true)
  const App = component(function* () {
    yield () => (mounted.value ? div(C()) : div())
  })

  render(App(), host)
  mounted.value = false
  flushSync()

  expect(cleaned).toBe(1)
})

test('this.provide above the yield reaches children mounted from the thunk', () => {
  const Theme = context<string>('theme')

  const Leaf = component(function* () {
    const theme = this.inject(Theme)
    yield () => span(theme)
  })

  const App = component(function* () {
    this.provide(Theme, 'dark')
    yield () => div(Leaf())
  })

  render(App(), host)
  expect(html()).toBe('<div><span>dark</span></div>')
})

test('a keyed list of parked components keeps state through a reorder', () => {
  const order = signal([1, 2, 3])

  const Row = component<{ id: number }>(function* (props) {
    const own = signal(0)
    yield () => li({ onclick: () => own.value++ }, `${props.id}:${own.value}`)
  })

  const App = component(function* () {
    yield () => ul(order.value.map((id) => Row({ key: id, id })))
  })

  render(App(), host)
  const first = host.querySelector('li')!
  first.dispatchEvent(new MouseEvent('click'))
  flushSync()
  expect(host.textContent).toBe('1:12:03:0')

  order.value = [3, 1, 2]
  flushSync()
  // Row 1 moved but kept its own signal.
  expect(host.textContent).toBe('3:01:12:0')
})

test('a yield* delegate can park its host, and still unwinds with it', () => {
  const order: string[] = []
  const n = signal(0)

  function* body(label: string): Behavior<unknown, never> {
    try {
      yield () => div(`${label}:${n.value}`)
      throw new Error('unreachable: the host parked on the thunk above')
    } finally {
      order.push('delegate finally')
    }
  }

  const Host = component(function* () {
    try {
      yield* body('x')
    } finally {
      order.push('host finally')
    }
  })

  const mounted = signal(true)
  const App = component(function* () {
    yield () => (mounted.value ? div(Host()) : div())
  })

  render(App(), host)
  expect(html()).toBe('<div><div>x:0</div></div>')

  n.value = 1
  flushSync()
  expect(html()).toBe('<div><div>x:1</div></div>')

  mounted.value = false
  flushSync()
  expect(order).toEqual(['delegate finally', 'host finally'])
})

// ---------------------------------------------------------------------------
// Error boundaries
// ---------------------------------------------------------------------------

test('a parked boundary catches a child throw and parks on its fallback', () => {
  const broken = signal(false)

  const Risky = component(function* () {
    yield () => {
      if (broken.value) throw new Error('boom')
      return span('fine')
    }
  })

  const retry = signal(0)
  const Boundary = component(function* () {
    try {
      yield () => div(Risky())
    } catch (error) {
      yield () => div(`caught ${String((error as Error).message)} ${retry.value}`)
    }
  })

  render(Boundary(), host)
  expect(html()).toBe('<div><span>fine</span></div>')

  broken.value = true
  flushSync()
  expect(html()).toBe('<div>caught boom 0</div>')

  // The fallback is itself a parked thunk, so it keeps updating from its deps.
  retry.value = 1
  flushSync()
  expect(html()).toBe('<div>caught boom 1</div>')
})

test('a thunk that throws is caught by its own enclosing try', () => {
  const broken = signal(false)

  const C = component(function* () {
    try {
      yield () => {
        if (broken.value) throw new Error('own throw')
        return span('fine')
      }
    } catch (error) {
      yield () => div(`caught ${String((error as Error).message)}`)
    }
  })

  render(C(), host)
  expect(html()).toBe('<span>fine</span>')

  broken.value = true
  flushSync()
  // A looping component catches its own view-building throws; a parked one has
  // to as well, or converting a loop to a thunk would silently drop the catch.
  expect(html()).toBe('<div>caught own throw</div>')
})

test('a catch yielding a plain view un-parks, and the loop retries', () => {
  const broken = signal(true)
  let attempts = 0

  const Risky = component(function* () {
    attempts++
    yield () => {
      if (broken.value) throw new Error('boom')
      return span('recovered')
    }
  })

  const Boundary = component(function* () {
    while (true) {
      try {
        yield () => div(Risky())
      } catch {
        // A plain view here: the component is generator-driven again, so
        // reaching the top of the loop is a real retry.
        yield div(span('fallback'), String(broken.value))
      }
    }
  })

  render(Boundary(), host)
  expect(html()).toBe('<div><span>fallback</span>true</div>')

  const before = attempts
  broken.value = false
  flushSync()
  expect(html()).toBe('<div><span>recovered</span></div>')
  expect(attempts).toBeGreaterThan(before)
})

test('an uncaught throw from a parked component reaches an ancestor', () => {
  const broken = signal(false)

  const Risky = component(function* () {
    yield () => {
      if (broken.value) throw new Error('boom')
      return span('fine')
    }
  })

  const Boundary = component(function* () {
    while (true) {
      try {
        yield div(Risky())
      } catch (error) {
        yield div(`ancestor caught ${String((error as Error).message)}`)
      }
    }
  })

  render(Boundary(), host)
  broken.value = true
  flushSync()
  expect(html()).toBe('<div>ancestor caught boom</div>')
})

test('finally still runs when a parked component is torn down by a throw', () => {
  const order: string[] = []
  const broken = signal(false)

  const Risky = component(function* () {
    try {
      yield () => {
        if (broken.value) throw new Error('boom')
        return span('fine')
      }
    } finally {
      order.push('risky finally')
    }
  })

  const Boundary = component(function* () {
    while (true) {
      try {
        yield div(Risky())
      } catch {
        yield div('caught')
      }
    }
  })

  render(Boundary(), host)
  broken.value = true
  flushSync()

  expect(html()).toBe('<div>caught</div>')
  expect(order).toEqual(['risky finally'])
})

// ---------------------------------------------------------------------------
// Async
// ---------------------------------------------------------------------------

test('an async prologue parks on a thunk and stops advancing', async () => {
  let resumes = 0
  const n = signal(0)

  const User = asyncComponent(async function* () {
    resumes++
    yield div('loading')
    await tick(10)
    yield () => div(`ada ${n.value}`)
  })

  render(User(), host)
  await tick(0)
  expect(html()).toBe('<div>loading</div>')

  await tick(40)
  expect(html()).toBe('<div>ada 0</div>')

  const settled = resumes

  // Parked: driven by signals now, not by the async loop.
  n.value = 1
  flushSync()
  expect(html()).toBe('<div>ada 1</div>')
  expect(resumes).toBe(settled)
})

test('reads in a parked thunk are tracked without this.track, even after an await', async () => {
  const n = signal(0)

  const C = asyncComponent(async function* () {
    await tick(5)
    // The synchronous window closed at the await, but the thunk gets a fresh
    // one on every call — so this read subscribes with no this.track().
    yield () => div(String(n.value))
  })

  render(C(), host)
  await tick(30)
  expect(html()).toBe('<div>0</div>')

  n.value = 7
  flushSync()
  expect(html()).toBe('<div>7</div>')
})

test('a parked async component cannot trip the spin guard', async () => {
  const C = asyncComponent(async function* () {
    // No await anywhere. As a loop this would spin to SPIN_LIMIT and throw.
    yield () => div('parked')
  })

  render(C(), host)
  await tick(50)
  expect(html()).toBe('<div>parked</div>')
})

test('props arriving while an async component is still advancing are coalesced', async () => {
  const seen: string[] = []
  const Child = asyncComponent<{ label: string }>(async function* (props) {
    await tick(20)
    yield () => {
      seen.push(props.label)
      return div(props.label)
    }
  })

  const label = signal('a')
  const App = component(function* () {
    yield () => div(Child({ label: label.value }))
  })

  render(App(), host)
  label.value = 'b'
  flushSync()
  label.value = 'c'
  flushSync()

  await tick(60)
  expect(html()).toBe('<div><div>c</div></div>')
})

test('unmounting a parked async component runs its cleanups', async () => {
  let cleaned = 0
  const C = asyncComponent(async function* () {
    using _cleanup1 = disposable(() => cleaned++)
    await tick(5)
    yield () => div('x')
  })

  const mounted = signal(true)
  const App = component(function* () {
    yield () => (mounted.value ? div(C()) : div())
  })

  render(App(), host)
  await tick(30)
  expect(html()).toBe('<div><div>x</div></div>')

  mounted.value = false
  flushSync()
  // An async generator's `return()` hands back a promise, so a `using` scope in
  // an async component unwinds a tick after the unmount rather than during it.
  await tick(0)
  expect(cleaned).toBe(1)
})

// ---------------------------------------------------------------------------
// Dev guards
// ---------------------------------------------------------------------------

let warn: ReturnType<typeof spyOn> | null = null

afterEach(() => {
  warn?.mockRestore()
  warn = null
})

test('an async render thunk throws rather than committing a promise', () => {
  const C = component(function* () {
    // Only reachable from JavaScript: this does not typecheck.
    yield (async () => div('x')) as unknown as () => View
  })

  expect(() => render(C(), host)).toThrow(/async render thunk/)
})

test('a thunk returning another function throws', () => {
  const C = component(function* () {
    yield (() => () => div('x')) as unknown as () => View
  })

  expect(() => render(C(), host)).toThrow(/returned another function/)
})

test('a function in a child position throws', () => {
  const C = component(function* () {
    yield () => div((() => span('x')) as unknown as View)
  })

  expect(() => render(C(), host)).toThrow(/function reached a child position/)
})

test('a thunk that reads nothing, after setup read something, warns once', () => {
  warn = spyOn(console, 'warn').mockImplementation(() => {})
  const seed = signal('x')

  const C = component(function* () {
    // No untrack(): this is the mistake the warning is for.
    const captured = seed.value
    yield () => div(captured)
  })

  render(C(), host)

  expect(warn).toHaveBeenCalledTimes(1)
  expect(String(warn.mock.calls[0]?.[0])).toMatch(/render thunk reads nothing/)
})

test('a thunk that reads something does not warn', () => {
  warn = spyOn(console, 'warn').mockImplementation(() => {})
  const seed = signal('x')

  const C = component(function* () {
    const prefix = seed.peek()
    yield () => div(prefix, seed.value)
  })

  render(C(), host)
  expect(warn).not.toHaveBeenCalled()
})

test('a static thunk with no setup reads does not warn', () => {
  warn = spyOn(console, 'warn').mockImplementation(() => {})

  const C = component(function* () {
    yield () => div('static')
  })

  render(C(), host)
  expect(warn).not.toHaveBeenCalled()
})
