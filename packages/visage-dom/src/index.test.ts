import { test, expect, beforeEach } from 'bun:test'
import {
  batch, component, computed, context, disposable, flushSync, keyed, render, signal, tags,
} from './index.ts'
import type { Behavior, ComponentGen, Ctx } from './index.ts'

/** Subscriber count. `subs` is internal to the signal graph, not public API. */
const subCount = (source: unknown): number =>
  (source as { subs: Set<unknown> }).subs.size


const { div, span, button, ul, li, p } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

const html = () => host.innerHTML.replace(/<!---->/g, '')

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

test('renders a static tree', () => {
  const App = component(function* () {
    yield () => div({ class: 'root' }, span('hello'), span('world'))
  })
  render(App(), host)
  expect(html()).toBe('<div class="root"><span>hello</span><span>world</span></div>')
})

test('renders text, numbers, and holes', () => {
  const App = component(function* () {
    yield () => div('a', 1, null, false, undefined, 'b')
  })
  render(App(), host)
  expect(host.textContent).toBe('a1b')
})

test('applies properties, style objects, dataset and attrs', () => {
  const App = component(function* () {
    yield () =>
      div({
        id: 'x',
        class: 'c',
        style: { color: 'red', paddingLeft: 4 },
        dataset: { role: 'main' },
        attrs: { 'aria-label': 'hi' },
      })
  })
  render(App(), host)
  const el = host.querySelector('div')!
  expect(el.id).toBe('x')
  expect(el.className).toBe('c')
  expect(el.style.color).toBe('red')
  expect(el.style.paddingLeft).toBe('4px')
  expect(el.dataset['role']).toBe('main')
  expect(el.getAttribute('aria-label')).toBe('hi')
})

// ---------------------------------------------------------------------------
// The yield boundary is the reactive boundary
// ---------------------------------------------------------------------------

test('a signal read between resume and yield becomes a dependency', () => {
  const count = signal(0)
  let renders = 0

  const Counter = component(function* () {
    while (true) {
      renders += 1
      yield div(`count: ${count.value}`)
    }
  })

  render(Counter(), host)
  expect(html()).toBe('<div>count: 0</div>')
  expect(renders).toBe(1)

  count.value = 5
  flushSync()
  expect(html()).toBe('<div>count: 5</div>')
  expect(renders).toBe(2)
})

test('a signal that is not read does not cause a re-render', () => {
  const used = signal(0)
  const unused = signal(0)
  let renders = 0

  const C = component(function* () {
    while (true) {
      renders += 1
      yield div(`${used.value}`)
    }
  })

  render(C(), host)
  expect(renders).toBe(1)

  unused.value = 99
  flushSync()
  expect(renders).toBe(1)

  used.value = 1
  flushSync()
  expect(renders).toBe(2)
})

test('dependencies are recomputed each yield, so they can change', () => {
  const toggle = signal(true)
  const a = signal('a')
  const b = signal('b')
  let renders = 0

  const C = component(function* () {
    while (true) {
      renders += 1
      yield div(toggle.value ? a.value : b.value)
    }
  })

  render(C(), host)
  expect(html()).toBe('<div>a</div>')

  // `b` is not a dependency yet.
  b.value = 'b2'
  flushSync()
  expect(renders).toBe(1)

  toggle.value = false
  flushSync()
  expect(html()).toBe('<div>b2</div>')

  // Now `a` is no longer a dependency.
  const before = renders
  a.value = 'a2'
  flushSync()
  expect(renders).toBe(before)
})

test('this.refresh() resumes without any signal', () => {
  let n = 0
  const C = component(function* () {
    while (true) {
      yield button({ onclick: () => { n += 1; this.refresh() } }, `n=${n}`)
    }
  })
  render(C(), host)
  expect(host.textContent).toBe('n=0')

  host.querySelector('button')!.click()
  flushSync()
  expect(host.textContent).toBe('n=1')
})

test('batch coalesces writes into one render', () => {
  const a = signal(0)
  const b = signal(0)
  let renders = 0
  const C = component(function* () {
    while (true) {
      renders += 1
      yield div(`${a.value}-${b.value}`)
    }
  })
  render(C(), host)
  expect(renders).toBe(1)

  batch(() => {
    a.value = 1
    b.value = 2
  })
  flushSync()
  expect(html()).toBe('<div>1-2</div>')
  expect(renders).toBe(2)
})

test('computed values track and invalidate', () => {
  const first = signal('ada')
  const last = signal('lovelace')
  const full = computed(() => `${first.value} ${last.value}`)

  const C = component(function* () {
    yield () => div(full.value)
  })
  render(C(), host)
  expect(html()).toBe('<div>ada lovelace</div>')

  first.value = 'grace'
  flushSync()
  expect(html()).toBe('<div>grace lovelace</div>')
})

// ---------------------------------------------------------------------------
// Typed props through yield
// ---------------------------------------------------------------------------

test('props arrive through yield and are typed', () => {
  interface Props { label: string }
  const seen: string[] = []

  const Child = component<Props>(function* (props) {
    while (true) {
      seen.push(props.label)
      yield div(props.label)
    }
  })

  const label = signal('first')
  const Parent = component(function* () {
    yield () => div(Child({ label: label.value }))
  })

  render(Parent(), host)
  expect(host.textContent).toBe('first')

  label.value = 'second'
  flushSync()
  expect(host.textContent).toBe('second')
  expect(seen).toEqual(['first', 'second'])
})

// ---------------------------------------------------------------------------
// yield* delegation
// ---------------------------------------------------------------------------

test('yield* delegates view and state to a behavior', () => {
  function* counterBehavior(step: number): Behavior<unknown, never> {
    let n = 0
    while (true) {
      yield div(`n=${n}`)
      n += step
    }
  }

  const C = component(function* () {
    yield* counterBehavior(2)
    void this
  })

  render(C(), host)
  expect(host.textContent).toBe('n=0')
})

test('a behavior disposes when its host unmounts', () => {
  const log: string[] = []

  function* withResource(name: string): Behavior<unknown> {
    try {
      yield () => div(name)
    } finally {
      log.push(`cleanup:${name}`)
    }
  }

  const C = component(function* () {
    yield* withResource('sub')
  })

  const root = render(C(), host)
  expect(host.textContent).toBe('sub')

  root.unmount()
  expect(log).toEqual(['cleanup:sub'])
})

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

test('using inside a component disposes on unmount', () => {
  const log: string[] = []
  const resource = (name: string) => ({
    [Symbol.dispose]() { log.push(name) },
  })

  const C = component(function* () {
    using a = resource('a')
    using b = resource('b')
    void a, void b
    yield () => div('x')
  })

  const root = render(C(), host)
  expect(log).toEqual([])
  root.unmount()
  expect(log).toEqual(['b', 'a'])
})

test('this.aborted fires on unmount', () => {
  let aborted = false
  const C = component(function* () {
    this.aborted.addEventListener('abort', () => { aborted = true })
    yield () => div('x')
  })
  const root = render(C(), host)
  expect(aborted).toBe(false)
  root.unmount()
  expect(aborted).toBe(true)
})

test('unmounting stops a components subscriptions', () => {
  const s = signal(0)
  let renders = 0
  const C = component(function* () {
    while (true) { renders += 1; yield div(`${s.value}`) }
  })
  const root = render(C(), host)
  expect(renders).toBe(1)
  root.unmount()
  s.value = 1
  flushSync()
  expect(renders).toBe(1)
})

// ---------------------------------------------------------------------------
// Keyed reconciliation
// ---------------------------------------------------------------------------

test('keyed lists reorder without recreating nodes', () => {
  const items = signal([{ id: 'a' }, { id: 'b' }, { id: 'c' }])

  const List = component(function* () {
    yield () =>
      ul(keyed(items.value, (i) => i.id, (i) => li({ id: i.id }, i.id)))
  })

  render(List(), host)
  const first = host.querySelector('#a')!
  expect([...host.querySelectorAll('li')].map((l) => l.id)).toEqual(['a', 'b', 'c'])

  items.value = [{ id: 'c' }, { id: 'a' }, { id: 'b' }]
  flushSync()
  expect([...host.querySelectorAll('li')].map((l) => l.id)).toEqual(['c', 'a', 'b'])
  // Same DOM node was moved, not recreated.
  expect(host.querySelector('#a')).toBe(first)
})

test('keyed lists handle insertion and removal', () => {
  const items = signal(['a', 'b', 'c'])
  const List = component(function* () {
    yield () => ul(keyed(items.value, (i) => i, (i) => li(i)))
  })
  render(List(), host)

  items.value = ['a', 'x', 'b', 'c']
  flushSync()
  expect([...host.querySelectorAll('li')].map((l) => l.textContent)).toEqual(['a', 'x', 'b', 'c'])

  items.value = ['x', 'c']
  flushSync()
  expect([...host.querySelectorAll('li')].map((l) => l.textContent)).toEqual(['x', 'c'])

  items.value = []
  flushSync()
  expect(host.querySelectorAll('li').length).toBe(0)
})

test('keyed component children keep their own state across reorder', () => {
  interface Props { name: string }
  const Item = component<Props>(function* (props) {
    const local = signal(0)
    yield () =>
      li(
        { id: props.name },
        button({ onclick: () => local.update((n) => n + 1) }, `${props.name}:${local.value}`),
      )
  })

  const order = signal(['a', 'b'])
  const List = component(function* () {
    yield () =>
      ul(order.value.map((n) => ({ ...Item({ name: n }), key: n })))
  })

  render(List(), host)
  host.querySelector('#a button')!.dispatchEvent(new Event('click'))
  flushSync()
  expect(host.querySelector('#a')!.textContent).toBe('a:1')

  order.value = ['b', 'a']
  flushSync()
  expect([...host.querySelectorAll('li')].map((l) => l.id)).toEqual(['b', 'a'])
  // 'a' kept its local state through the move.
  expect(host.querySelector('#a')!.textContent).toBe('a:1')
})

// ---------------------------------------------------------------------------
// Conditionals and structure changes
// ---------------------------------------------------------------------------

test('swapping node types replaces in place', () => {
  const mode = signal<'p' | 'span'>('p')
  const C = component(function* () {
    yield () => div(mode.value === 'p' ? p('para') : span('span'))
  })
  render(C(), host)
  expect(html()).toBe('<div><p>para</p></div>')

  mode.value = 'span'
  flushSync()
  expect(html()).toBe('<div><span>span</span></div>')
})

test('conditional children keep sibling positions stable', () => {
  const show = signal(false)
  const C = component(function* () {
    yield () => div(span('before'), show.value ? span('mid') : null, span('after'))
  })
  render(C(), host)
  expect(host.textContent).toBe('beforeafter')

  show.value = true
  flushSync()
  expect(host.textContent).toBe('beforemidafter')

  show.value = false
  flushSync()
  expect(host.textContent).toBe('beforeafter')
})

test('unmounting a subtree disposes nested components', () => {
  const log: string[] = []
  const Leaf = component(function* () {
    using _leaf = disposable(() => log.push('leaf'))
    yield () => span('leaf')
  })
  const Middle = component(function* () {
    using _middle = disposable(() => log.push('middle'))
    yield () => div(Leaf())
  })
  const show = signal(true)
  const Root = component(function* () {
    yield () => div(show.value ? Middle() : null)
  })

  render(Root(), host)
  show.value = false
  flushSync()
  // Child before parent, matching Solid, React and Vue: a child's teardown may
  // touch a resource its parent owns, never the other way round.
  expect(log).toEqual(['leaf', 'middle'])
})

// ---------------------------------------------------------------------------
// Scheduling
// ---------------------------------------------------------------------------

test('parents flush before children', () => {
  const order: string[] = []
  const s = signal(0)

  const Child = component(function* () {
    while (true) { order.push('child'); yield span(`${s.value}`) }
  })
  const Parent = component(function* () {
    while (true) { order.push('parent'); yield div(`${s.value}`, Child()) }
  })

  render(Parent(), host)
  order.length = 0
  s.value = 1
  flushSync()
  expect(order[0]).toBe('parent')
})

test('a component that finishes stops updating but keeps its view', () => {
  const s = signal(0)
  const Once = component(function* () {
    yield div(`once:${s.value}`)
  })
  render(Once(), host)
  expect(html()).toBe('<div>once:0</div>')

  s.value = 1
  flushSync()
  expect(html()).toBe('<div>once:0</div>')
})

// ---------------------------------------------------------------------------
// Refs
// ---------------------------------------------------------------------------

test('ref receives the element', () => {
  let captured: HTMLElement | null = null
  const C = component(function* () {
    yield () => div({ ref: (el) => { captured = el } }, 'x')
  })
  render(C(), host)
  expect(captured).not.toBeNull()
  expect(captured!.tagName).toBe('DIV')
})

// ---------------------------------------------------------------------------
// Type-level check that the generator return type is usable
// ---------------------------------------------------------------------------

test('ComponentGen is assignable for an explicitly annotated component', () => {
  interface P { v: number }
  const C = component<P>(function* (props): ComponentGen {
    yield () => div(`${props.v}`)
  })
  render(C({ v: 3 }), host)
  expect(host.textContent).toBe('3')
})

test('a duplicate key does not orphan a live component', () => {
  const disposed: string[] = []
  const tick = signal(0)
  const renders: string[] = []

  const Item = component<{ label: string }>(function* (props) {
    const own = props.label
    using _tracked = { [Symbol.dispose]: () => disposed.push(own) }
    while (true) {
      yield () => {
        renders.push(`${own}:${String(tick.value)}`)
        return li(own)
      }
    }
  })

  // Two children share key 'k1'. Nothing warns about this today, and the
  // reconciler used to let both claim the same new slot — the loser landed in
  // neither the result nor the removals, so it was never disposed, its node
  // was never removed, and it kept re-rendering forever.
  const rows = signal<Array<{ key: string; label: string }>>([
    { key: 'k1', label: 'a' },
    { key: 'k1', label: 'b' },
    { key: 'k2', label: 'c' },
  ])

  const List = component(function* () {
    yield () => ul(rows.value.map((r) => Item({ key: r.key, label: r.label })))
  })

  render(List(), host)
  expect(host.querySelectorAll('li').length).toBe(3)

  // No common prefix or suffix, so the whole list goes through the key map.
  rows.value = [
    { key: 'k2', label: 'c' },
    { key: 'k1', label: 'a' },
  ]
  flushSync()

  expect(host.querySelectorAll('li').length).toBe(2)
  expect(disposed).toContain('b')

  // The orphan used to stay subscribed and re-render alongside the survivors.
  renders.length = 0
  tick.value = 1
  flushSync()
  expect(renders.length).toBe(2)
})

test('a yield inside a finally still lets enclosing using scopes unwind', () => {
  const disposed: string[] = []

  const Comp = component(function* () {
    using _outer = { [Symbol.dispose]: () => disposed.push('outer') }
    try {
      while (true) yield div('x')
    } finally {
      // Legal, and it makes the first `gen.return()` come back `{done: false}`.
      // The generator stays suspended right here, so `_outer` never unwound.
      yield div('goodbye')
    }
  })

  const root = render(Comp(), host)
  root.unmount()

  expect(disposed).toEqual(['outer'])
})

test('a component killed by a render throw stops subscribing', () => {
  const s = signal(0)

  // Mounts cleanly and subscribes to `s`, then throws on the resume `s` causes.
  // Throwing during mount would prove nothing: `#resumeSync` never reaches
  // `resubscribe`, so there is no subscription to leave behind.
  const Boom = component(function* () {
    while (true) {
      if (s.value > 0) throw new Error('boom')
      yield div('ok')
    }
  })

  // No boundary on purpose. A boundary would replace the broken subtree, and
  // the unmount would unsubscribe it anyway — it is the component that dies
  // *without* being unmounted that stays in the subscriber set forever.
  const App = component(function* () {
    yield () => div(Boom())
  })

  render(App(), host)
  expect(subCount(s)).toBe(1)

  s.value = 1
  // `flush` reports per-subscriber errors, so the unhandled throw does not
  // escape here; the component is simply dead afterwards.
  flushSync()

  // `#fail` used to leave the dead component in every source's subscriber set
  // until something got round to unmounting it.
  expect(subCount(s)).toBe(0)
})

test('flushSync from inside a render body does not kill the component', () => {
  const a = signal(0)

  const C = component(function* () {
    while (true) {
      const v = a.value
      if (v === 1) {
        // Schedules this same component, then drains the queue re-entrantly.
        a.value = 2
        flushSync()
      }
      yield div(String(v))
    }
  })

  render(C(), host)
  expect(host.textContent).toBe('0')

  a.value = 1
  // The re-entrant resume used to clear the in-progress dep set and then throw
  // `TypeError: Generator is executing`, which `run` read as a render failure
  // and turned into a dead component.
  flushSync()
  expect(host.textContent).toBe('2')

  // Still alive and still subscribed.
  a.value = 3
  flushSync()
  expect(host.textContent).toBe('3')
})

test('a ref can return a cleanup, which runs on unmount', () => {
  const log: string[] = []
  const show = signal(true)

  const C = component(function* () {
    yield () =>
      div(
        show.value
          ? span({
              ref: (el) => {
                log.push(`attach:${el.tagName}`)
                return () => log.push('detach')
              },
            })
          : null,
      )
  })

  const root = render(C(), host)
  expect(log).toEqual(['attach:SPAN'])

  // A ref that stores the element anywhere the component does not own is the
  // detached-node leak, and `using` cannot reach it — the reconciler calls the
  // ref, not the component. This is the only way to undo it.
  show.value = false
  flushSync()
  expect(log).toEqual(['attach:SPAN', 'detach'])

  root.unmount()
  expect(log).toEqual(['attach:SPAN', 'detach'])
})

test('a ref cleanup runs when the whole root unmounts', () => {
  const log: string[] = []
  const C = component(function* () {
    yield () => div(span({ ref: () => () => log.push('detach') }))
  })

  const root = render(C(), host)
  root.unmount()
  expect(log).toEqual(['detach'])
})

test('a ref may be a concise arrow whose body returns something', () => {
  // `push` returns a number. Typing `ref` as `void | (() => void)` rejected
  // this — a plain `void` return is what keeps the ordinary form spellable
  // while still allowing a cleanup to come back.
  const seen: HTMLElement[] = []
  const C = component(function* () {
    yield () => div({ ref: (el) => seen.push(el) })
  })

  const root = render(C(), host)
  expect(seen.length).toBe(1)
  expect(() => root.unmount()).not.toThrow()
})

test('a ref that returns nothing is still fine', () => {
  let seen: HTMLElement | null = null
  const C = component(function* () {
    yield () => div({ ref: (el) => { seen = el } })
  })
  const root = render(C(), host)
  expect(seen).not.toBeNull()
  expect(() => root.unmount()).not.toThrow()
})

// ---------------------------------------------------------------------------
// The `this` binding
// ---------------------------------------------------------------------------

test('a yield* delegate reaches the context through .call(this)', () => {
  const Theme = context<string>('theme')

  // A behavior is an ordinary generator function, so it gets its own `this` —
  // the one thing `this` does not do is cross a call boundary. Declaring the
  // parameter is what makes the requirement visible: TypeScript rejects a bare
  // `yield* withTheme()` rather than letting it read `undefined` at runtime.
  function* withTheme(this: Ctx): Behavior<unknown, string> {
    return this.inject(Theme)
  }

  const Leaf = component(function* () {
    const theme = yield* withTheme.call(this)
    yield () => span(theme)
  })

  const App = component(function* () {
    this.provide(Theme, 'delegated')
    yield () => div(Leaf())
  })

  render(App(), host)
  expect(host.textContent).toBe('delegated')
})

test('a plain helper takes the context as an ordinary argument', () => {
  // The shape every `use*` in visage-router has: `this` at the call site, a
  // normal parameter in the helper.
  const Theme = context<string>('theme')
  const useTheme = (ctx: Ctx): string => ctx.inject(Theme)

  const Leaf = component(function* () {
    const theme = useTheme(this)
    yield () => span(theme)
  })

  const App = component(function* () {
    this.provide(Theme, 'threaded')
    yield () => div(Leaf())
  })

  render(App(), host)
  expect(host.textContent).toBe('threaded')
})
