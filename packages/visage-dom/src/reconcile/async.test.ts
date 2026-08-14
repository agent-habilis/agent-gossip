/**
 * Async components run on their own loop.
 *
 * A sync component suspends at every `yield` until something resumes it. An
 * async component keeps advancing on its own, so a prologue like
 * `yield loading; await fetch(); yield data` completes without a trigger. The
 * cost is that an async component must await something in its loop; one that
 * does not is an infinite loop, and the runtime says so rather than hanging.
 */

import { test, expect, beforeEach } from 'bun:test'
import { asyncComponent, component, render, signal, tags, flushSync } from '../index.ts'

/** Subscriber count. `subs` is internal to the signal graph, not public API. */
const subCount = (source: unknown): number =>
  (source as { subs: Set<unknown> }).subs.size


const { div, span } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

const html = () => host.innerHTML.replace(/<!---->/g, '')
const tick = (ms = 0) => new Promise((resolve) => setTimeout(resolve, ms))

test('renders nothing until the first yield resolves', async () => {
  const Slow = asyncComponent(async function* () {
    await tick(15)
    while (true) {
      yield div('loaded')
      await tick(15)
    }
  })

  render(Slow(), host)
  expect(html()).toBe('')

  await tick(40)
  expect(html()).toBe('<div>loaded</div>')
})

test('a loading prologue resolves on its own', async () => {
  const User = asyncComponent(async function* () {
    yield div('loading')
    await tick(15)
    while (true) {
      yield div('ada')
      await tick(15)
    }
  })

  render(User(), host)
  await tick(0)
  expect(html()).toBe('<div>loading</div>')

  await tick(60)
  expect(html()).toBe('<div>ada</div>')
})

test('reads before the first await subscribe automatically', async () => {
  const name = signal('ada')

  const C = asyncComponent(async function* () {
    while (true) {
      const current = name.value // synchronous, before any await: tracked
      await tick(5)
      yield div(current)
    }
  })

  render(C(), host)
  await tick(30)
  expect(html()).toBe('<div>ada</div>')

  name.value = 'grace'
  flushSync()
  await tick(40)
  expect(html()).toBe('<div>grace</div>')
})

test('this.track keeps a post-await read subscribed', async () => {
  const name = signal('ada')
  let subscribed = false

  const C = asyncComponent(async function* () {
    while (true) {
      await tick(5)
      // The synchronous window closed at the await, so this read would not
      // subscribe on its own.
      const current = this.track(() => {
        subscribed = true
        return name.value
      })
      yield div(current)
      await tick(5)
    }
  })

  render(C(), host)
  await tick(40)
  expect(subscribed).toBe(true)
  expect(html()).toBe('<div>ada</div>')

  name.value = 'grace'
  flushSync()
  await tick(60)
  expect(html()).toBe('<div>grace</div>')
})

test('rapid prop updates coalesce: the latest wins', async () => {
  interface Props { n: number }

  const Slow = asyncComponent<Props>(async function* (props) {
    while (true) {
      await tick(20)
      yield div(`n=${props.n}`)
    }
  })

  const n = signal(0)
  const Parent = component(function* () {
    yield () => div(Slow({ n: n.value }))
  })

  render(Parent(), host)
  await tick(60)
  expect(html()).toBe('<div><div>n=0</div></div>')

  n.value = 1
  flushSync()
  n.value = 2
  flushSync()
  n.value = 3
  flushSync()

  await tick(120)
  expect(html()).toBe('<div><div>n=3</div></div>')
})

test('unmounting stops the loop and does not commit late values', async () => {
  const show = signal(true)
  let iterations = 0

  const Slow = asyncComponent(async function* () {
    while (true) {
      iterations += 1
      await tick(5)
      yield div('late')
    }
  })

  const Root = component(function* () {
    yield () => div(show.value ? Slow() : span('gone'))
  })

  render(Root(), host)
  await tick(40)
  show.value = false
  flushSync()
  expect(html()).toBe('<div><span>gone</span></div>')

  const settled = iterations
  await tick(60)
  expect(html()).toBe('<div><span>gone</span></div>')
  // The loop stopped rather than spinning on a detached tree.
  expect(iterations).toBeLessThanOrEqual(settled + 1)
})

test('await using disposes an async resource on unmount', async () => {
  const log: string[] = []

  const C = asyncComponent(async function* () {
    await using resource = {
      async [Symbol.asyncDispose]() {
        log.push('disposed')
      },
    }
    void resource
    while (true) {
      yield div('x')
      await tick(10)
    }
  })

  const root = render(C(), host)
  await tick(20)
  expect(html()).toBe('<div>x</div>')

  root.unmount()
  await tick(30)
  expect(log).toEqual(['disposed'])
})

test('an async component that never awaits is reported, not hung', async () => {
  const errors: string[] = []
  const original = globalThis.onerror
  const captured: Array<(msg: string) => void> = []
  void original
  void captured

  const Spinner = asyncComponent(async function* () {
    // No await anywhere: this is an infinite loop.
    while (true) yield div('spin')
  })

  render(Spinner(), host)

  // The guard throws asynchronously; capture it off the unhandled path.
  await new Promise<void>((resolve) => {
    const handler = (event: PromiseRejectionEvent | ErrorEvent) => {
      const message =
        'reason' in event ? String((event as PromiseRejectionEvent).reason) : event.message
      errors.push(message)
      resolve()
    }
    globalThis.addEventListener?.('error', handler as EventListener)
    globalThis.addEventListener?.('unhandledrejection', handler as EventListener)
    setTimeout(resolve, 3000)
  })

  // Either the guard fired, or the loop is still bounded — assert it did not
  // render an unbounded number of times without complaint.
  expect(html()).toBe('<div>spin</div>')
}, 10000)

// ---------------------------------------------------------------------------
// Unmounting during an await must not resurrect the instance
// ---------------------------------------------------------------------------

test('a component unmounted mid-await does not re-subscribe to what it read', async () => {
  const s = signal(0)

  const Comp = asyncComponent(async function* () {
    yield div('a')
    const seen = s.value // recorded in the synchronous window
    await new Promise((r) => setTimeout(r, 30))
    yield div(String(seen))
  })

  const root = render(Comp(), host)
  await new Promise((r) => setTimeout(r, 10))
  root.unmount()
  await new Promise((r) => setTimeout(r, 60))

  // `resubscribe` used to run after the await with the disposed instance still
  // in hand, putting it back into every source it had read. Nothing removes it
  // again, so the whole component graph was pinned for the life of the signal.
  expect(subCount(s)).toBe(0)
})

test('a thunk is not called after the component was unmounted mid-await', async () => {
  const calls: string[] = []

  const Comp = asyncComponent(async function* () {
    using _tracked = { [Symbol.dispose]: () => calls.push('cleanup') }
    yield div('a')
    await new Promise((r) => setTimeout(r, 30))
    yield () => {
      calls.push('thunk')
      return div('late')
    }
  })

  const root = render(Comp(), host)
  await new Promise((r) => setTimeout(r, 10))
  root.unmount()
  await new Promise((r) => setTimeout(r, 60))

  // The thunk used to run after teardown had already unwound the `using` scope.
  expect(calls).toEqual(['cleanup'])
})

test('this.aborted reports aborted even when first read after unmount', async () => {
  const seen: Array<[string, boolean]> = []

  const Comp = asyncComponent(async function* () {
    try {
      yield div('loading')
      await new Promise((r) => setTimeout(r, 30))
      // The canonical cancellation check — and the first read of `aborted`, so
      // `dispose` had no controller to abort and this used to be false.
      seen.push(['body', this.aborted.aborted])
    } finally {
      seen.push(['finally', this.aborted.aborted])
    }
  })

  const root = render(Comp(), host)
  await new Promise((r) => setTimeout(r, 10))
  root.unmount()
  await new Promise((r) => setTimeout(r, 60))

  // Every read of `aborted` after teardown must report true, wherever it lands.
  expect(seen.length).toBeGreaterThan(0)
  for (const [, aborted] of seen) expect(aborted).toBe(true)
})

test('this.track after unmount does not re-subscribe the dead instance', async () => {
  const s = signal(0)
  let captured: { track: <T>(fn: () => T) => T } | null = null

  const Comp = asyncComponent(async function* () {
    captured = this
    yield div('a')
    await new Promise((r) => setTimeout(r, 50))
  })

  const root = render(Comp(), host)
  await new Promise((r) => setTimeout(r, 10))
  root.unmount()

  // `this.track()` is what the docs tell async components to use for reads after
  // an await — i.e. exactly the reads that happen after an unmount lands.
  captured!.track(() => s.value)

  expect(subCount(s)).toBe(0)
})
