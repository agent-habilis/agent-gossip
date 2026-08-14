import { test, expect, beforeEach } from 'bun:test'
import { browserHistory, hashHistory, memoryHistory } from './index.ts'
import type { NavigationAction } from './index.ts'

/** Collect what a history reports, so assertions read as a navigation log. */
function log(): {
  entries: string[]
  listen: (to: string, action: NavigationAction) => void
} {
  const entries: string[] = []
  return { entries, listen: (to, action) => entries.push(`${action} ${to}`) }
}

beforeEach(() => {
  // The DOM backends share one address bar, so put it back between tests.
  globalThis.history.replaceState(null, '', '/')
})

// ---------------------------------------------------------------------------
// memoryHistory
// ---------------------------------------------------------------------------

test('memory starts at the last entry unless told otherwise', () => {
  expect(memoryHistory().current).toBe('/')
  expect(memoryHistory(['/a', '/b']).current).toBe('/b')
  expect(memoryHistory(['/a', '/b'], 0).current).toBe('/a')
})

test('memory rejects an empty stack and an out-of-range index', () => {
  expect(() => memoryHistory([])).toThrow(/at least one entry/)
  expect(() => memoryHistory(['/a'], 3)).toThrow(/outside its 1 entries/)
})

test('memory normalizes entries that arrive without a leading slash', () => {
  const history = memoryHistory(['a/b'])
  expect(history.current).toBe('/a/b')
  history.push('c')
  expect(history.current).toBe('/c')
})

test('push and replace differ in whether they add an entry', () => {
  const history = memoryHistory()
  history.push('/a')
  history.push('/b')
  history.replace('/c')
  expect(history.current).toBe('/c')
  history.go(-1)
  // Two pushes made two entries; the replace overwrote the second.
  expect(history.current).toBe('/a')
})

test('pushing after going back drops the forward entries', () => {
  const history = memoryHistory(['/a', '/b', '/c'])
  history.go(-2)
  expect(history.current).toBe('/a')
  history.push('/d')
  history.go(1)
  // /b and /c are gone, so there is nothing ahead of /d to go to.
  expect(history.current).toBe('/d')
})

test('go clamps at both ends and stays silent when it cannot move', () => {
  const history = memoryHistory(['/a', '/b'])
  const { entries, listen } = log()
  history.subscribe(listen)

  history.go(5)
  history.go(-5)
  history.go(0)
  expect(entries).toEqual([])
  expect(history.current).toBe('/b')

  history.go(-1)
  expect(entries).toEqual(['pop /a'])
})

test('a push mints a new key and a replace keeps the current one', () => {
  const history = memoryHistory()
  const first = history.key
  history.push('/a')
  const second = history.key
  expect(second).not.toBe(first)
  history.replace('/b')
  // Same entry, so the same key — which is what lets a replace keep the
  // scroll position the router saved against it.
  expect(history.key).toBe(second)
})

test('going back restores the key that entry was created with', () => {
  const history = memoryHistory()
  const first = history.key
  history.push('/a')
  expect(history.key).not.toBe(first)
  history.go(-1)
  expect(history.key).toBe(first)
})

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

test('every navigation reports its action', () => {
  const history = memoryHistory()
  const { entries, listen } = log()
  history.subscribe(listen)

  history.push('/a')
  history.replace('/b')
  history.go(-1)

  expect(entries).toEqual(['push /a', 'replace /b', 'pop /'])
})

test('unsubscribing stops delivery, and doing it twice is harmless', () => {
  const history = memoryHistory()
  const { entries, listen } = log()
  const off = history.subscribe(listen)

  history.push('/a')
  off()
  off()
  history.push('/b')

  expect(entries).toEqual(['push /a'])
})

test('a listener may unsubscribe while being notified', () => {
  const history = memoryHistory()
  const seen: string[] = []
  const off = history.subscribe((to) => {
    seen.push(to)
    off()
  })
  history.subscribe((to) => seen.push(`second ${to}`))

  history.push('/a')
  history.push('/b')

  // The first listener saw only /a, and removing itself mid-walk did not stop
  // the second from being called.
  expect(seen).toEqual(['/a', 'second /a', 'second /b'])
})

test('the DOM backend binds no listener until somebody subscribes', () => {
  const history = browserHistory()
  const { entries, listen } = log()

  // Constructing a history is what a test does dozens of times; it must not
  // leave a popstate handler on window behind.
  const off = history.subscribe(listen)
  history.push('/a')
  expect(entries).toEqual(['push /a'])

  off()
  history.push('/b')
  expect(entries).toEqual(['push /a'])
})

// ---------------------------------------------------------------------------
// browserHistory
// ---------------------------------------------------------------------------

test('browser reads the pathname, search and hash together', () => {
  globalThis.history.replaceState(null, '', '/users/2?tab=a#top')
  expect(browserHistory().current).toBe('/users/2?tab=a#top')
})

test('browser writes through to the address bar', () => {
  const history = browserHistory()
  history.push('/users/2?tab=a')
  expect(globalThis.location.pathname).toBe('/users/2')
  expect(globalThis.location.search).toBe('?tab=a')
  expect(history.current).toBe('/users/2?tab=a')
})

test('browser stamps a key onto an entry it did not create', () => {
  globalThis.history.replaceState(null, '', '/cold')
  const history = browserHistory()
  expect(history.key).not.toBe('')
  // The stamp went into history.state, so a later read finds the same key.
  expect(browserHistory().key).toBe(history.key)
})

test('browser keeps caller state separate from its own bookkeeping', () => {
  const history = browserHistory()
  history.push('/a', { scrollTo: 'top' })
  const state = globalThis.history.state as { key: string; usr: unknown }
  expect(state.usr).toEqual({ scrollTo: 'top' })
  expect(state.key).toBe(history.key)
})

test('browser accepts state that cannot be spread', () => {
  const history = browserHistory()
  // The reason user state is nested rather than merged into the entry object.
  history.push('/a', 'just a string')
  expect((globalThis.history.state as { usr: unknown }).usr).toBe('just a string')
})

// ---------------------------------------------------------------------------
// hashHistory
// ---------------------------------------------------------------------------

test('hash reads the fragment as the whole router path', () => {
  globalThis.history.replaceState(null, '', '/examples#/7guis/cells?x=1')
  expect(hashHistory().current).toBe('/7guis/cells?x=1')
})

test('hash treats a missing fragment as the root', () => {
  globalThis.history.replaceState(null, '', '/examples')
  expect(hashHistory().current).toBe('/')
})

test('hash writes the path into the fragment and leaves the pathname alone', () => {
  globalThis.history.replaceState(null, '', '/examples')
  const history = hashHistory()
  history.push('/todo')
  expect(globalThis.location.pathname).toBe('/examples')
  expect(globalThis.location.hash).toBe('#/todo')
  expect(history.current).toBe('/todo')
})

test('hash can replace an entry, which assigning location.hash cannot', () => {
  const history = hashHistory()
  history.push('/a')
  const key = history.key
  history.replace('/b')
  expect(history.current).toBe('/b')
  expect(history.key).toBe(key)
})

// ---------------------------------------------------------------------------
// base
// ---------------------------------------------------------------------------

test('base is removed on read and put back on write', () => {
  globalThis.history.replaceState(null, '', '/app/users/2?tab=a')
  const history = browserHistory({ base: '/app' })
  expect(history.current).toBe('/users/2?tab=a')

  history.push('/settings')
  expect(globalThis.location.pathname).toBe('/app/settings')
  expect(history.current).toBe('/settings')
})

test('the root of a based app is the base itself', () => {
  globalThis.history.replaceState(null, '', '/app')
  const history = browserHistory({ base: '/app' })
  expect(history.current).toBe('/')

  history.push('/')
  expect(globalThis.location.pathname).toBe('/app')
})

test('a base is accepted with or without its slashes', () => {
  globalThis.history.replaceState(null, '', '/app/x')
  for (const base of ['/app', 'app', '/app/']) {
    expect(browserHistory({ base }).current).toBe('/x')
  }
})

test('a path outside the base is handed back untouched', () => {
  globalThis.history.replaceState(null, '', '/elsewhere/x')
  // Guessing here would silently mangle the path; reporting it as-is lets the
  // router fall through to its 404 instead.
  expect(browserHistory({ base: '/app' }).current).toBe('/elsewhere/x')
})

test('a base of / means no base at all', () => {
  globalThis.history.replaceState(null, '', '/x')
  expect(browserHistory({ base: '/' }).current).toBe('/x')
})

test('base applies inside the fragment in hash mode', () => {
  globalThis.history.replaceState(null, '', '/host#/app/todo')
  const history = hashHistory({ base: '/app' })
  expect(history.current).toBe('/todo')

  history.push('/snake')
  expect(globalThis.location.hash).toBe('#/app/snake')
})
