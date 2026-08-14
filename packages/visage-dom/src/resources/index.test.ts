import { test, expect, beforeEach } from 'bun:test'
import {
  abortable,
  animation,
  asyncDisposable,
  component,
  disposable,
  interval,
  listen,
  observe,
  poll,
  render,
  tags,
  timeout,
} from '../index.ts'
import type { Behavior } from '../index.ts'

const { div } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

const tick = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms))

// ---------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------

test('interval fires until disposed', async () => {
  let ticks = 0
  const timer = interval(10, () => { ticks += 1 })

  await tick(45)
  expect(ticks).toBeGreaterThanOrEqual(2)

  const settled = ticks
  timer[Symbol.dispose]()
  await tick(40)
  expect(ticks).toBe(settled)
})

test('timeout is cancelled if disposed before it fires', async () => {
  let fired = false
  const t = timeout(30, () => { fired = true })
  t[Symbol.dispose]()

  await tick(60)
  expect(fired).toBe(false)
})

test('timeout still fires when left alone', async () => {
  let fired = false
  timeout(10, () => { fired = true })
  await tick(40)
  expect(fired).toBe(true)
})

test('animation runs frames and reports a delta', async () => {
  const deltas: number[] = []
  const loop = animation((delta) => { deltas.push(delta) })

  await tick(60)
  loop[Symbol.dispose]()
  const settled = deltas.length

  expect(settled).toBeGreaterThan(0)
  expect(deltas[0]).toBe(0) // first frame has no previous

  await tick(40)
  expect(deltas.length).toBe(settled)
})

test('disposal is idempotent', async () => {
  let ticks = 0
  const timer = interval(10, () => { ticks += 1 })
  timer[Symbol.dispose]()
  timer[Symbol.dispose]()
  timer[Symbol.dispose]()

  await tick(40)
  expect(ticks).toBe(0)
})

// ---------------------------------------------------------------------------
// Polling
// ---------------------------------------------------------------------------

test('poll runs immediately by default', async () => {
  let runs = 0
  const p = poll(1000, () => { runs += 1 })
  await tick(10)
  p[Symbol.dispose]()
  expect(runs).toBe(1)
})

test('poll can wait out the first interval instead', async () => {
  let runs = 0
  const p = poll(1000, () => { runs += 1 }, { immediate: false })
  await tick(20)
  p[Symbol.dispose]()
  expect(runs).toBe(0)
})

test('poll never overlaps when the work is slower than the interval', async () => {
  let active = 0
  let maxActive = 0
  let completed = 0

  const p = poll(5, async () => {
    active += 1
    maxActive = Math.max(maxActive, active)
    await tick(30) // much slower than the 5ms interval
    active -= 1
    completed += 1
  })

  await tick(150)
  p[Symbol.dispose]()

  // An `interval` would have stacked these up; poll waits for each to settle.
  expect(maxActive).toBe(1)
  expect(completed).toBeGreaterThanOrEqual(2)
})

test('poll aborts the in-flight run on dispose', async () => {
  let aborted = false

  const p = poll(10, async (signal) => {
    signal.addEventListener('abort', () => { aborted = true })
    await tick(200)
  })

  await tick(20)
  p[Symbol.dispose]()
  expect(aborted).toBe(true)
})

test('poll reports errors through onError and keeps going', async () => {
  const errors: unknown[] = []
  let runs = 0

  const p = poll(
    10,
    () => {
      runs += 1
      throw new Error(`boom ${runs}`)
    },
    { onError: (error) => errors.push(error) },
  )

  await tick(60)
  p[Symbol.dispose]()

  expect(errors.length).toBeGreaterThanOrEqual(2)
  expect(String(errors[0])).toContain('boom 1')
})

test('poll does not report the abort it caused itself', async () => {
  const errors: unknown[] = []
  const p = poll(
    10,
    async (signal) => {
      await new Promise((resolve, reject) => {
        signal.addEventListener('abort', () => reject(new Error('aborted')))
        setTimeout(resolve, 500)
      })
    },
    { onError: (error) => errors.push(error) },
  )

  await tick(20)
  p[Symbol.dispose]()
  await tick(30)
  expect(errors).toEqual([])
})

// ---------------------------------------------------------------------------
// DOM subscriptions
// ---------------------------------------------------------------------------

test('listen attaches and removes an event listener', () => {
  const button = document.createElement('button')
  host.appendChild(button)

  let clicks = 0
  const subscription = listen(button, 'click', () => { clicks += 1 })

  button.dispatchEvent(new Event('click'))
  expect(clicks).toBe(1)

  subscription[Symbol.dispose]()
  button.dispatchEvent(new Event('click'))
  expect(clicks).toBe(1)
})

test('observe connects an observer and disconnects it on dispose', () => {
  const log: string[] = []
  const fake = {
    observe(node: Node) { log.push(`observe:${(node as Element).tagName}`) },
    disconnect() { log.push('disconnect') },
  }

  const target = document.createElement('span')
  const observer = observe(fake, target)
  expect(log).toEqual(['observe:SPAN'])

  observer[Symbol.dispose]()
  observer[Symbol.dispose]()
  expect(log).toEqual(['observe:SPAN', 'disconnect'])
})

// ---------------------------------------------------------------------------
// Generic primitives
// ---------------------------------------------------------------------------

test('disposable wraps a cleanup function and runs it once', () => {
  let runs = 0
  const d = disposable(() => { runs += 1 })
  d[Symbol.dispose]()
  d[Symbol.dispose]()
  expect(runs).toBe(1)
})

test('asyncDisposable runs once', async () => {
  let runs = 0
  const d = asyncDisposable(async () => { runs += 1 })
  await d[Symbol.asyncDispose]()
  await d[Symbol.asyncDispose]()
  expect(runs).toBe(1)
})

test('abortable aborts its signal on dispose', () => {
  const controller = abortable()
  expect(controller.signal.aborted).toBe(false)
  controller[Symbol.dispose]()
  expect(controller.signal.aborted).toBe(true)
})

test('DisposableStack disposes in reverse order of registration', () => {
  const log: string[] = []
  const s = new DisposableStack()
  s.use(disposable(() => log.push('a')))
  s.use(disposable(() => log.push('b')))
  s.defer(() => log.push('c'))

  s.dispose()
  expect(log).toEqual(['c', 'b', 'a'])

  // Idempotent, so a stray second dispose cannot re-run a teardown.
  s.dispose()
  expect(log).toEqual(['c', 'b', 'a'])
  expect(s.disposed).toBe(true)
})

test('DisposableStack.adopt wraps a value that is not itself disposable', () => {
  const log: string[] = []
  const s = new DisposableStack()
  const socket = s.adopt({ id: 7 }, (value) => log.push(`close:${String(value.id)}`))
  expect(socket.id).toBe(7)

  s.dispose()
  expect(log).toEqual(['close:7'])
})

// ---------------------------------------------------------------------------
// Integration with component lifetime
// ---------------------------------------------------------------------------

test('using interval stops when the component unmounts', async () => {
  let ticks = 0

  const Ticker = component(function* () {
    using timer = interval(10, () => { ticks += 1 })
    void timer
    yield () => div('tick')
  })

  const root = render(Ticker(), host)
  await tick(45)
  expect(ticks).toBeGreaterThanOrEqual(2)

  root.unmount()
  const settled = ticks
  await tick(40)
  expect(ticks).toBe(settled)
})

test('a resource acquired before the first yield lives until unmount', async () => {
  let ticks = 0

  const Ticker = component(function* () {
    using _timer = interval(10, () => { ticks += 1 })
    yield () => div('tick')
  })

  const root = render(Ticker(), host)
  await tick(45)
  expect(ticks).toBeGreaterThanOrEqual(2)

  root.unmount()
  const settled = ticks
  await tick(40)
  expect(ticks).toBe(settled)
})

test('a resource held by a yield* delegate is released with its host', async () => {
  let ticks = 0

  function* ticking(): Behavior<unknown> {
    using timer = interval(10, () => { ticks += 1 })
    void timer
    yield () => div('from behavior')
  }

  const Host = component(function* () {
    yield* ticking()
  })

  const root = render(Host(), host)
  await tick(45)
  expect(ticks).toBeGreaterThanOrEqual(2)

  root.unmount()
  const settled = ticks
  await tick(40)
  expect(ticks).toBe(settled)
})

test('a stack of resources is released on unmount, in reverse', async () => {
  const log: string[] = []

  const C = component(function* () {
    using resources = new DisposableStack()
    resources.use(interval(1000, () => {}))
    resources.defer(() => log.push('first'))
    resources.defer(() => log.push('second'))
    yield () => div('x')
  })

  const root = render(C(), host)
  root.unmount()
  await tick(10)
  expect(log).toEqual(['second', 'first'])
})

test('poll inside a component stops on unmount', async () => {
  let runs = 0

  const C = component(function* () {
    using p = poll(10, () => { runs += 1 })
    void p
    yield () => div('x')
  })

  const root = render(C(), host)
  await tick(45)
  expect(runs).toBeGreaterThanOrEqual(2)

  root.unmount()
  const settled = runs
  await tick(40)
  expect(runs).toBe(settled)
})

// ---------------------------------------------------------------------------
// Polyfill
// ---------------------------------------------------------------------------

test('the runtime ships Symbol.dispose and DisposableStack natively', () => {
  expect(typeof DisposableStack).toBe('function')
  expect(typeof Symbol.dispose).toBe('symbol')
  expect(typeof Symbol.asyncDispose).toBe('symbol')
})


test('poll keeps running when onError itself throws', async () => {
  let runs = 0
  using _p = poll(
    5,
    () => {
      runs += 1
      throw new Error('run failed')
    },
    {
      onError: () => {
        throw new Error('reporter failed')
      },
    },
  )

  await tick(60)
  // The throw from `onError` used to escape `run` before it could reschedule,
  // killing the poll permanently while its Disposable still looked healthy.
  expect(runs).toBeGreaterThan(3)
})

test('observe forwards an init, so MutationObserver works', () => {
  const target = document.createElement('div')
  let observed = false

  // Without an init `MutationObserver.observe` throws outright, which the
  // "works with any observer" claim used to walk straight into.
  using _mo = observe(new MutationObserver(() => { observed = true }), target, {
    childList: true,
  })

  target.appendChild(document.createElement('span'))
  expect(observed).toBe(false) // delivered on a microtask, not synchronously
})

test('observe does not put Symbol.dispose on the caller observer', () => {
  const ro = new MutationObserver(() => {})
  using _handle = observe(ro, document.createElement('div'), { childList: true })

  // It used to be assigned onto the observer itself, so any other holder gained
  // a dispose method and could tear down this site's resource.
  expect(Symbol.dispose in ro).toBe(false)
})

test('animation stops rescheduling once fn throws', async () => {
  let calls = 0
  const original = globalThis.onerror
  globalThis.onerror = null

  using _loop = animation(() => {
    calls += 1
    throw new Error('frame failed')
  })

  await tick(60)
  globalThis.onerror = original
  // It used to re-arm before calling `fn`, so a throwing callback threw on
  // every frame for the life of the page.
  expect(calls).toBe(1)
})
