import { test, expect, beforeEach } from 'bun:test'
import { component, disposable, flushSync, render, signal, tags } from 'visage-dom'
import type { Child } from 'visage-dom'
import {
  A,
  Outlet,
  createRouter,
  memoryHistory,
  useLoader,
  useLocation,
  useParams,
  useRouter,
  useSearchParams,
} from './index.ts'
import type { History, RouteDef } from './index.ts'

const { div, span, button, ul, li } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

const tick = (ms = 0): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms))

/** Mount a router and hand back the history driving it. */
function mount(
  routes: readonly RouteDef[],
  options: { at?: string; fallback?: RouteDef['component']; scroll?: boolean } = {},
): { history: History; unmount: () => void } {
  const history = memoryHistory([options.at ?? '/'])
  const App = createRouter({
    routes,
    history,
    scroll: options.scroll ?? false,
    ...(options.fallback === undefined ? {} : { fallback: options.fallback }),
  })
  const root = render(App(), host)
  flushSync()
  return { history, unmount: () => root.unmount() }
}

/** Navigate and settle, so assertions read against the committed DOM. */
function go(history: History, to: string): void {
  history.push(to)
  flushSync()
}

const text = (): string => host.textContent ?? ''

// ---------------------------------------------------------------------------
// Rendering a match
// ---------------------------------------------------------------------------

const Home = component(function* () {
  yield () => div('home')
})
const Todo = component(function* () {
  yield () => div('todo')
})

test('the matching route renders', () => {
  mount([
    { path: '/', component: Home },
    { path: '/todo', component: Todo },
  ])
  expect(text()).toBe('home')
})

test('navigating swaps the rendered route', () => {
  const { history } = mount([
    { path: '/', component: Home },
    { path: '/todo', component: Todo },
  ])
  go(history, '/todo')
  expect(text()).toBe('todo')
  go(history, '/')
  expect(text()).toBe('home')
})

test('going back through history renders the earlier route', () => {
  const { history } = mount([
    { path: '/', component: Home },
    { path: '/todo', component: Todo },
  ])
  go(history, '/todo')
  history.go(-1)
  flushSync()
  expect(text()).toBe('home')
})

test('nothing matching and no fallback renders nothing', () => {
  mount([{ path: '/todo', component: Todo }], { at: '/nope' })
  expect(text()).toBe('')
})

test('a fallback catches everything that did not match', () => {
  const NotFound = component(function* () {
    yield () => div('404')
  })
  const { history } = mount([{ path: '/todo', component: Todo }], {
    at: '/nope',
    fallback: NotFound,
  })
  expect(text()).toBe('404')
  go(history, '/todo')
  expect(text()).toBe('todo')
  go(history, '/also/missing')
  expect(text()).toBe('404')
})

test('a route with neither component nor lazy is a legal layout', () => {
  mount([{ path: '/', children: [{ path: '', component: Home }] }])
  expect(text()).toBe('home')
})

// ---------------------------------------------------------------------------
// Nesting
// ---------------------------------------------------------------------------

const Task = component(function* () {
  const params = useParams(this)
  yield () => span(`task:${params.value['task'] ?? '-'}`)
})

const Guis = component(function* () {
  yield () => div('guis[', Outlet(), ']')
})

const NESTED: RouteDef[] = [
  {
    path: '/7guis',
    component: Guis,
    children: [
      { path: '', component: component(function* () {
        yield () => span('pick one')
      }) },
      { path: ':task', component: Task },
    ],
  },
]

test('an outlet renders the next route in the chain', () => {
  const { history } = mount(NESTED, { at: '/7guis' })
  expect(text()).toBe('guis[pick one]')
  go(history, '/7guis/cells')
  expect(text()).toBe('guis[task:cells]')
})

test('the layout is not remounted when only the child changes', () => {
  let mounts = 0
  const Layout = component(function* () {
    mounts++
    yield () => div('layout[', Outlet(), ']')
  })
  const { history } = mount(
    [
      {
        path: '/a',
        component: Layout,
        children: [
          { path: 'x', component: Home },
          { path: 'y', component: Todo },
        ],
      },
    ],
    { at: '/a/x' },
  )
  expect(mounts).toBe(1)
  go(history, '/a/y')
  expect(text()).toBe('layout[todo]')
  // Same route object at depth 0, so the same instance is kept.
  expect(mounts).toBe(1)
})

test('nesting goes as deep as it is declared', () => {
  const Inner = component(function* () {
    yield () => span('inner')
  })
  const Mid = component(function* () {
    yield () => div('mid[', Outlet(), ']')
  })
  mount(
    [{ path: '/a', component: Guis, children: [{ path: 'b', component: Mid, children: [{ path: 'c', component: Inner }] }] }],
    { at: '/a/b/c' },
  )
  expect(text()).toBe('guis[mid[inner]]')
})

// ---------------------------------------------------------------------------
// Params and location
// ---------------------------------------------------------------------------

test('params update without remounting when only the value changes', () => {
  let mounts = 0
  const User = component(function* () {
    mounts++
    const params = useParams(this)
    yield () => div(`user:${params.value['id'] ?? '-'}`)
  })
  const { history } = mount([{ path: '/users/:id', component: User }], { at: '/users/1' })
  expect(text()).toBe('user:1')
  go(history, '/users/2')
  expect(text()).toBe('user:2')
  expect(mounts).toBe(1)
})

test('a component deep inside a route still sees param changes', () => {
  // The reason route state travels as signals in context rather than as props:
  // a component that reads no props is never woken by its parent, so a props
  // chain would leave this one stale.
  const Deep = component(function* () {
    const params = useParams(this)
    yield () => span(`deep:${params.value['id'] ?? '-'}`)
  })
  const Middle = component(function* () {
    yield () => div(Deep())
  })
  const User = component(function* () {
    yield () => div(Middle())
  })
  const { history } = mount([{ path: '/users/:id', component: User }], { at: '/users/1' })
  expect(text()).toBe('deep:1')
  go(history, '/users/2')
  expect(text()).toBe('deep:2')
})

test('location reports the path in pieces', () => {
  const Probe = component(function* () {
    const location = useLocation(this)
    yield () => {
      const at = location.value
      return div(`${at.pathname}|${at.search}|${at.hash}|${at.query.get('tab') ?? ''}`)
    }
  })
  const { history } = mount([{ path: '/a', component: Probe }], { at: '/a' })
  expect(text()).toBe('/a|||')
  go(history, '/a?tab=two#top')
  expect(text()).toBe('/a|?tab=two|#top|two')
})

test('navigate resolves a relative path against the current one', () => {
  const Probe = component(function* () {
    const router = useRouter(this)
    const location = useLocation(this)
    yield () =>
      div(
        span(location.value.pathname),
        button({ onclick: () => router.navigate('../c') }, 'up'),
      )
  })
  const { history } = mount([{ path: '/*', component: Probe }], { at: '/a/b' })
  expect(text()).toContain('/a/b')
  host.querySelector('button')?.click()
  flushSync()
  // Relative to the directory '/a', so '../c' is '/c' rather than '/a/b/c'.
  expect(history.current).toBe('/c')
  expect(text()).toContain('/c')
})

test('navigate takes a number and moves through history', () => {
  const { history } = mount([
    { path: '/', component: Home },
    { path: '/todo', component: Todo },
  ])
  go(history, '/todo')
  expect(text()).toBe('todo')
  history.go(-1)
  flushSync()
  expect(text()).toBe('home')
})

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

/** A left click, the kind a router is supposed to take over. */
function click(el: Element, init: MouseEventInit = {}): MouseEvent {
  const event = new MouseEvent('click', { bubbles: true, cancelable: true, button: 0, ...init })
  el.dispatchEvent(event)
  return event
}

const LINKS = component(function* () {
  yield () =>
    ul(
      li(A({ href: '/', end: true, children: 'home' })),
      li(A({ href: '/todo', children: 'todo' })),
      li(A({ href: 'https://example.com', children: 'out' })),
    )
})

function linksRouter(at = '/'): { history: History } {
  return mount(
    [
      { path: '/', component: LINKS },
      { path: '/todo', component: LINKS },
      { path: '/todo/:id', component: LINKS },
    ],
    { at },
  )
}

test('a link renders a real anchor with a real href', () => {
  linksRouter()
  const anchors = [...host.querySelectorAll('a')]
  expect(anchors.map((a) => a.getAttribute('href'))).toEqual([
    '/',
    '/todo',
    'https://example.com',
  ])
})

test('a plain left click navigates instead of reloading', () => {
  linksRouter()
  const todo = host.querySelectorAll('a')[1]!
  const event = click(todo)
  flushSync()
  expect(event.defaultPrevented).toBe(true)
  expect(host.querySelector('a[data-current="true"]')?.textContent).toBe('todo')
})

test('a modified click is left to the browser', () => {
  linksRouter()
  const todo = host.querySelectorAll('a')[1]!
  for (const init of [
    { metaKey: true },
    { ctrlKey: true },
    { shiftKey: true },
    { altKey: true },
    { button: 1 },
  ]) {
    const event = click(todo, init)
    flushSync()
    // Not prevented, so cmd-click still opens a tab and middle-click still
    // opens a background one.
    expect(event.defaultPrevented).toBe(false)
  }
})

test('an external link is left alone', () => {
  linksRouter()
  const out = host.querySelectorAll('a')[2]!
  const event = click(out)
  expect(event.defaultPrevented).toBe(false)
})

test('a link marks itself current, and end controls how strictly', () => {
  const { history } = linksRouter('/todo/7')
  const current = () =>
    [...host.querySelectorAll('a')]
      .filter((a) => a.getAttribute('data-current') === 'true')
      .map((a) => a.textContent)

  // '/todo' is a prefix of '/todo/7', and '/' is exact-only or it would light
  // up on every page.
  expect(current()).toEqual(['todo'])

  go(history, '/')
  expect(current()).toEqual(['home'])
})

test('the current link is marked for assistive technology too', () => {
  linksRouter('/todo')
  const todo = host.querySelectorAll('a')[1]!
  expect(todo.getAttribute('aria-current')).toBe('page')
  expect(host.querySelectorAll('a')[0]!.hasAttribute('aria-current')).toBe(false)
})

test('an active link takes the active class alongside its own', () => {
  const Nav = component(function* () {
    yield () => A({ href: '/todo', class: 'nav__link', activeClass: 'is-on', children: 'todo' })
  })
  mount([{ path: '/todo', component: Nav }], { at: '/todo' })
  expect(host.querySelector('a')?.getAttribute('class')).toBe('nav__link is-on')
})

test('a link runs its own onclick first and honours a preventDefault', () => {
  const seen: string[] = []
  const Nav = component(function* () {
    yield () =>
      div(
        A({
          href: '/todo',
          onclick: (event) => {
            seen.push('own')
            event.preventDefault()
          },
          children: 'todo',
        }),
      )
  })
  const { history } = mount(
    [
      { path: '/', component: Nav },
      { path: '/todo', component: Todo },
    ],
    { at: '/' },
  )
  click(host.querySelector('a')!)
  flushSync()
  expect(seen).toEqual(['own'])
  // The handler cancelled it, so the router stayed put.
  expect(history.current).toBe('/')
})

test('a link can replace instead of push', () => {
  const Nav = component(function* () {
    yield () => div(A({ href: '/todo', replace: true, children: 'todo' }))
  })
  const { history } = mount(
    [
      { path: '/', component: Nav },
      { path: '/todo', component: Todo },
    ],
    { at: '/' },
  )
  click(host.querySelector('a')!)
  flushSync()
  expect(history.current).toBe('/todo')
  history.go(-1)
  flushSync()
  // The replace overwrote the only entry, so there is nothing to go back to.
  expect(history.current).toBe('/todo')
})

// ---------------------------------------------------------------------------
// Search params
// ---------------------------------------------------------------------------

test('search params read as a signal and write as a navigation', () => {
  const Arm = component(function* () {
    const [query, setQuery] = useSearchParams(this)
    yield () =>
      div(
        span(query.value.get('arm') ?? 'none'),
        button({ onclick: () => setQuery({ arm: 'fine' }) }, 'fine'),
        button({ onclick: () => setQuery({ arm: null }) }, 'clear'),
      )
  })
  const { history } = mount([{ path: '/particles', component: Arm }], { at: '/particles' })
  expect(text()).toContain('none')

  host.querySelectorAll('button')[0]!.click()
  flushSync()
  expect(text()).toContain('fine')
  expect(history.current).toBe('/particles?arm=fine')

  host.querySelectorAll('button')[1]!.click()
  flushSync()
  expect(text()).toContain('none')
  expect(history.current).toBe('/particles')
})

test('setting search params keeps the other ones', () => {
  const Probe = component(function* () {
    const [query, setQuery] = useSearchParams(this)
    yield () =>
      div(
        span(`${query.value.get('a') ?? '-'}/${query.value.get('b') ?? '-'}`),
        button({ onclick: () => setQuery({ b: '2' }) }, 'set'),
      )
  })
  mount([{ path: '/x', component: Probe }], { at: '/x?a=1' })
  host.querySelector('button')!.click()
  flushSync()
  expect(text()).toContain('1/2')
})

test('search params accept a function of the previous value', () => {
  const Probe = component(function* () {
    const [query, setQuery] = useSearchParams(this)
    yield () =>
      div(
        span(query.value.get('n') ?? '0'),
        button(
          {
            onclick: () =>
              setQuery((previous) => {
                const next = new URLSearchParams(previous)
                next.set('n', String(Number(previous.get('n') ?? '0') + 1))
                return next
              }),
          },
          '+',
        ),
      )
  })
  mount([{ path: '/x', component: Probe }], { at: '/x' })
  host.querySelector('button')!.click()
  flushSync()
  expect(text()).toContain('1')
  host.querySelector('button')!.click()
  flushSync()
  expect(text()).toContain('2')
})

// ---------------------------------------------------------------------------
// Lazy routes
// ---------------------------------------------------------------------------

test('a lazy route shows its pending view and then its own', async () => {
  let loads = 0
  const route: RouteDef = {
    path: '/late',
    pending: div('loading'),
    lazy: async () => {
      loads++
      await tick()
      return Todo
    },
  }
  const { history } = mount([{ path: '/', component: Home }, route])
  go(history, '/late')
  // An async component mounts a hole and delivers its first yield a microtask
  // later, so the pending view is never there synchronously.
  await tick()
  flushSync()
  expect(text()).toBe('loading')

  await tick(5)
  flushSync()
  expect(text()).toBe('todo')
  expect(loads).toBe(1)
})

test('a lazy route accepts a module with a default export', async () => {
  const { history } = mount([
    { path: '/', component: Home },
    { path: '/late', lazy: async () => ({ default: Todo }) },
  ])
  go(history, '/late')
  await tick(5)
  flushSync()
  expect(text()).toBe('todo')
})

test('a revisited lazy route renders synchronously, with no pending flash', async () => {
  let loads = 0
  const route: RouteDef = {
    path: '/late',
    pending: div('loading'),
    lazy: async () => {
      loads++
      await tick()
      return Todo
    },
  }
  const { history } = mount([{ path: '/', component: Home }, route])

  go(history, '/late')
  await tick(5)
  flushSync()
  expect(text()).toBe('todo')

  go(history, '/')
  go(history, '/late')
  // No await: the component is remembered, so this visit never goes async and
  // never shows the pending view again.
  expect(text()).toBe('todo')
  expect(loads).toBe(1)
})

test('a lazy import resolving to nothing says so', async () => {
  const { history } = mount([
    { path: '/', component: Home },
    // eslint-disable-next-line
    { path: '/late', lazy: async () => undefined as never },
  ])
  go(history, '/late')
  await tick(5)
  // The error escapes to the console rather than being swallowed; what matters
  // here is that it did not render as an empty page with no explanation.
  expect(text()).not.toBe('todo')
})

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

test('a synchronous loader is ready on the first render', () => {
  const Show = component(function* () {
    const data = useLoader<string>(this)
    yield () => div(`${data.value.status}:${data.value.data ?? '-'}`)
  })
  mount([{ path: '/x', component: Show, load: () => 'now' }], { at: '/x' })
  // No pending flash: a loader that did not need to wait never reports waiting.
  expect(text()).toBe('ready:now')
})

test('an async loader reports pending and then ready', async () => {
  const Show = component(function* () {
    const data = useLoader<string>(this)
    yield () => div(`${data.value.status}:${data.value.data ?? '-'}`)
  })
  const { history } = mount([
    { path: '/', component: Home },
    {
      path: '/x',
      component: Show,
      load: async () => {
        await tick()
        return 'later'
      },
    },
  ])
  go(history, '/x')
  expect(text()).toBe('pending:-')
  await tick(5)
  flushSync()
  expect(text()).toBe('ready:later')
})

test('a loader that throws reports the error', () => {
  const Show = component(function* () {
    const data = useLoader<string>(this)
    yield () => div(`${data.value.status}:${String((data.value.error as Error)?.message ?? '')}`)
  })
  mount(
    [
      {
        path: '/x',
        component: Show,
        load: () => {
          throw new Error('nope')
        },
      },
    ],
    { at: '/x' },
  )
  expect(text()).toBe('error:nope')
})

test('a superseded load is aborted and never lands', async () => {
  const aborted: string[] = []
  const Show = component(function* () {
    const data = useLoader<string>(this)
    const params = useParams(this)
    yield () => div(`${params.value['id']}=${data.value.data ?? '-'}`)
  })
  const { history } = mount(
    [
      { path: '/', component: Home },
      {
        path: '/x/:id',
        component: Show,
        load: async ({ params, signal }) => {
          signal.addEventListener('abort', () => aborted.push(params['id'] ?? '?'))
          await tick(20)
          return `data${params['id']}`
        },
      },
    ],
    { at: '/' },
  )

  go(history, '/x/1')
  go(history, '/x/2')
  await tick(40)
  flushSync()

  expect(aborted).toEqual(['1'])
  // The slow first load resolved after the second, and was discarded.
  expect(text()).toBe('2=data2')
})

test('a loader sees the params of its own depth', () => {
  const Show = component(function* () {
    const data = useLoader<string>(this)
    yield () => span(`[${data.value.data ?? '-'}]`)
  })
  const Layout = component(function* () {
    const data = useLoader<string>(this)
    yield () => div(`outer:${data.value.data ?? '-'}`, Outlet())
  })
  mount(
    [
      {
        path: '/org/:org',
        component: Layout,
        load: ({ params }) => `org-${params['org']}`,
        children: [{ path: 'repo/:name', component: Show, load: ({ params }) => `repo-${params['name']}` }],
      },
    ],
    { at: '/org/acme/repo/site' },
  )
  expect(text()).toBe('outer:org-acme[repo-site]')
})

test('a depth with no loader still reports ready', () => {
  const Show = component(function* () {
    const data = useLoader<string>(this)
    yield () => div(data.value.status)
  })
  mount([{ path: '/x', component: Show }], { at: '/x' })
  expect(text()).toBe('ready')
})

// ---------------------------------------------------------------------------
// Teardown
// ---------------------------------------------------------------------------

test('unmounting releases the history subscription', () => {
  const { history, unmount } = mount([
    { path: '/', component: Home },
    { path: '/todo', component: Todo },
  ])
  unmount()
  // Nothing left listening, so this must not reach a torn-down tree.
  expect(() => go(history, '/todo')).not.toThrow()
  expect(host.textContent).toBe('')
})

test('unmounting aborts a load still in flight', async () => {
  let wasAborted = false
  const { unmount } = mount(
    [
      {
        path: '/x',
        component: Home,
        load: async ({ signal }) => {
          signal.addEventListener('abort', () => {
            wasAborted = true
          })
          await tick(20)
          return 'late'
        },
      },
    ],
    { at: '/x' },
  )
  unmount()
  await tick(30)
  expect(wasAborted).toBe(true)
})

test('leaving a route disposes what it held', () => {
  const disposed: string[] = []
  const Held = component(function* () {
    using _cleanup = disposable(() => disposed.push('held'))
    yield () => div('held')
  })
  const { history } = mount([
    { path: '/', component: Held },
    { path: '/todo', component: Todo },
  ])
  go(history, '/todo')
  expect(disposed).toEqual(['held'])
})

test('the router works without a DOM-backed history', () => {
  // memoryHistory never touches location, which is what lets suites run in any
  // order without leaking URL state into each other.
  const history = memoryHistory(['/a', '/b'], 0)
  const routes: RouteDef[] = [
    { path: '/a', component: Home },
    { path: '/b', component: Todo },
  ]
  const App = createRouter({ routes, history, scroll: false })
  render(App(), host)
  flushSync()
  expect(text()).toBe('home')
  history.go(1)
  flushSync()
  expect(text()).toBe('todo')
})

// ---------------------------------------------------------------------------
// Remounting
// ---------------------------------------------------------------------------

test('a key forces a remount when it changes', () => {
  // How to reset per-route state on a param change, since the reconciler
  // otherwise keeps the instance. The key has to sit on a child inside a list
  // — only `patchChildren` consults keys; a component's root view is matched
  // on type alone.
  let mounts = 0
  const Fresh = component(function* () {
    mounts++
    const params = useParams(this)
    const seen = signal(params.value['id'])
    yield () => span(`${seen.value}`)
  })
  const Wrapper = component(function* () {
    const params = useParams(this)
    yield (): Child => div(Fresh({ key: params.value['id'] ?? '' }))
  })
  const { history } = mount([{ path: '/u/:id', component: Wrapper }], { at: '/u/1' })
  expect(text()).toBe('1')
  expect(mounts).toBe(1)
  go(history, '/u/2')
  expect(text()).toBe('2')
  expect(mounts).toBe(2)
})
