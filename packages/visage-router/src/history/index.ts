/**
 * Where the URL lives.
 *
 * Three backends behind one interface — the real address bar, the fragment
 * after `#`, and an array in memory:
 *
 *     const history = browserHistory()
 *     history.subscribe((to, action) => console.log(action, to))
 *     history.push('/users/2')
 *
 * This module imports nothing — not the reconciler, not the element factory —
 * which is why `subscribe` returns a plain unsubscribe function rather than a
 * `Disposable`. `DisposableStack.adopt` and a bare `finally` both take exactly
 * that, so the router pays no ergonomic price, and the file stays usable from a
 * test, a server, or another framework entirely.
 *
 * The two DOM backends both drive `pushState`/`replaceState` and both listen to
 * `popstate`. Hash mode differs only in where it writes the path — into the
 * fragment rather than the pathname — and in also watching `hashchange`, which
 * is the one thing `popstate` misses when a user edits the fragment by hand.
 * Assigning `location.hash` directly would have been shorter, but it cannot
 * carry per-entry state and it cannot replace an entry, and both of those are
 * load-bearing for scroll restoration.
 */

/** How the current entry was arrived at. */
export type NavigationAction = 'push' | 'replace' | 'pop'

export type NavigationListener = (to: string, action: NavigationAction) => void

export interface History {
  /** Path, search and hash, with `base` removed. Always starts with `/`. */
  readonly current: string
  /**
   * Identifies the current entry, for callers keying anything to it — scroll
   * positions, most usefully. A push mints a new one; a replace keeps the
   * existing one, because it stays on the same entry.
   */
  readonly key: string
  push(to: string, state?: unknown): void
  replace(to: string, state?: unknown): void
  /** Move through the stack. Negative goes back. */
  go(delta: number): void
  /**
   * What a router path looks like in an `href`. Hash mode answers `#/todo`
   * where browser mode answers `/todo`, which is the whole reason links can be
   * real anchors under either backend.
   */
  href(to: string): string
  /** Returns the unsubscribe function. Calling it twice is harmless. */
  subscribe(fn: NavigationListener): () => void
}

export interface HistoryOptions {
  /**
   * A path prefix the app is mounted under. `/app` means the address bar reads
   * `/app/users/2` while `current` reads `/users/2`, so routes never have to
   * know where they were deployed.
   */
  base?: string
}

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/** Split a full path into its pathname and the `?…#…` remainder. */
function splitPath(full: string): [pathname: string, rest: string] {
  const at = full.search(/[?#]/)
  return at === -1 ? [full, ''] : [full.slice(0, at), full.slice(at)]
}

function normalize(to: string): string {
  return to.startsWith('/') ? to : `/${to}`
}

/** `'/app/'` and `'app'` and `'/app'` all mean the same thing; `'/'` means none. */
function normalizeBase(base: string | undefined): string {
  if (base === undefined || base === '' || base === '/') return ''
  const lead = base.startsWith('/') ? base : `/${base}`
  return lead.endsWith('/') ? lead.slice(0, -1) : lead
}

function withoutBase(full: string, base: string): string {
  if (base === '') return full
  const [pathname, rest] = splitPath(full)
  if (pathname === base) return `/${rest}`
  if (pathname.startsWith(`${base}/`)) return pathname.slice(base.length) + rest
  // Outside the base entirely. Hand it back untouched rather than guess.
  return full
}

function withBase(full: string, base: string): string {
  if (base === '') return full
  const [pathname, rest] = splitPath(full)
  return `${base}${pathname === '/' ? '' : pathname}${rest}`
}

// ---------------------------------------------------------------------------
// Listeners
// ---------------------------------------------------------------------------

interface Listeners {
  size(): number
  add(fn: NavigationListener): () => void
  emit(to: string, action: NavigationAction): void
}

/**
 * `onFirst`/`onLast` let a backend bind its DOM listener only while somebody is
 * actually watching, so constructing a history — which tests do constantly —
 * never leaves a `popstate` handler behind on `window`.
 */
function listeners(onFirst?: () => void, onLast?: () => void): Listeners {
  const set = new Set<NavigationListener>()
  return {
    size: () => set.size,
    add(fn) {
      set.add(fn)
      if (set.size === 1) onFirst?.()
      let done = false
      return () => {
        if (done) return
        done = true
        set.delete(fn)
        if (set.size === 0) onLast?.()
      }
    },
    emit(to, action) {
      // Copy first: a listener is allowed to unsubscribe during the walk.
      for (const fn of [...set]) fn(to, action)
    },
  }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/**
 * Entry keys have to survive a reload, so they live in `history.state` rather
 * than in a counter. User state is nested under `usr` instead of merged, so a
 * caller pushing a string — or anything else that does not spread — still works.
 */
interface Entry {
  key: string
  usr?: unknown
}

function createKey(): string {
  return Math.random().toString(36).slice(2, 10)
}

function readEntry(): Entry | undefined {
  const state = globalThis.history.state as Partial<Entry> | null | undefined
  return typeof state?.key === 'string' ? (state as Entry) : undefined
}

// ---------------------------------------------------------------------------
// The DOM backends
// ---------------------------------------------------------------------------

interface DomBackend {
  /** The current router path, read out of `location`. */
  read(): string
  /** Turn a router path into something `pushState` can take as its URL. */
  toUrl(path: string): string
  /** Whether to also watch `hashchange`. */
  watchHash: boolean
}

function domHistory(backend: DomBackend): History {
  let key = readEntry()?.key ?? createKey()
  let last = backend.read()

  // A cold load lands on an entry the router never stamped. Stamp it now, or
  // navigating away and back finds no key and cannot restore its scroll.
  if (readEntry() === undefined) {
    globalThis.history.replaceState({ key } satisfies Entry, '')
  }

  const onPop = (): void => {
    key = readEntry()?.key ?? createKey()
    last = backend.read()
    subs.emit(last, 'pop')
  }

  const onHashChange = (): void => {
    // `popstate` fires for back/forward through hash entries too, so this only
    // has to catch the case it misses: the fragment edited in the address bar.
    const now = backend.read()
    if (now === last) return
    key = createKey()
    last = now
    subs.emit(now, 'pop')
  }

  const subs = listeners(
    () => {
      globalThis.addEventListener('popstate', onPop)
      if (backend.watchHash) globalThis.addEventListener('hashchange', onHashChange)
    },
    () => {
      globalThis.removeEventListener('popstate', onPop)
      if (backend.watchHash) globalThis.removeEventListener('hashchange', onHashChange)
    },
  )

  const write = (to: string, state: unknown, action: 'push' | 'replace'): void => {
    const path = normalize(to)
    // A push starts a new entry, so it needs its own key. A replace stays on
    // the current one, and keeping the key keeps its saved scroll position.
    if (action === 'push') key = createKey()
    const entry: Entry = state === undefined ? { key } : { key, usr: state }
    const url = backend.toUrl(path)
    if (action === 'push') globalThis.history.pushState(entry, '', url)
    else globalThis.history.replaceState(entry, '', url)
    last = backend.read()
    subs.emit(last, action)
  }

  return {
    get current() {
      return backend.read()
    },
    get key() {
      return key
    },
    push: (to, state) => write(to, state, 'push'),
    replace: (to, state) => write(to, state, 'replace'),
    go: (delta) => globalThis.history.go(delta),
    href: (to) => backend.toUrl(normalize(to)),
    subscribe: (fn) => subs.add(fn),
  }
}

/** The address bar. Needs the server to serve the app for unknown paths. */
export function browserHistory(options?: HistoryOptions): History {
  const base = normalizeBase(options?.base)
  return domHistory({
    read: () => {
      const { pathname, search, hash } = globalThis.location
      return withoutBase(`${pathname}${search}${hash}`, base)
    },
    toUrl: (path) => withBase(path, base),
    watchHash: false,
  })
}

/**
 * Everything after `#`. No server configuration at all, which is what makes it
 * the right choice for `file://` and for static hosts with no rewrite rule.
 */
export function hashHistory(options?: HistoryOptions): History {
  const base = normalizeBase(options?.base)
  return domHistory({
    read: () => {
      const raw = globalThis.location.hash.slice(1)
      return withoutBase(raw === '' ? '/' : normalize(raw), base)
    },
    // Relative, so the pathname and search of the real URL are left alone.
    toUrl: (path) => `#${withBase(path, base)}`,
    watchHash: true,
  })
}

// ---------------------------------------------------------------------------
// The memory backend
// ---------------------------------------------------------------------------

/**
 * A stack in an array. Tests use this one: it never touches `location`, so
 * suites cannot leak URL state into each other and can run in any order.
 */
export function memoryHistory(
  entries: readonly string[] = ['/'],
  index: number = entries.length - 1,
): History {
  if (entries.length === 0) {
    throw new Error('visage-router: memoryHistory needs at least one entry.')
  }
  if (index < 0 || index >= entries.length) {
    throw new Error(
      `visage-router: memoryHistory index ${index} is outside its ${entries.length} ` +
        'entries. Pass an index into the array, or leave it off to start at the end.',
    )
  }

  const stack = entries.map(normalize)
  const keys = entries.map(createKey)
  let at = index

  const subs = listeners()
  const read = (): string => stack[at] ?? '/'

  return {
    get current() {
      return read()
    },
    get key() {
      return keys[at] ?? ''
    },
    push(to) {
      // Anything ahead of the current entry is unreachable once you push, the
      // same way it is in a real session.
      stack.splice(at + 1)
      keys.splice(at + 1)
      stack.push(normalize(to))
      keys.push(createKey())
      at = stack.length - 1
      subs.emit(read(), 'push')
    },
    replace(to) {
      stack[at] = normalize(to)
      subs.emit(read(), 'replace')
    },
    go(delta) {
      const next = at + delta
      // Real history clamps rather than throwing; a step past either end is
      // simply not taken, and no event fires.
      if (next < 0 || next >= stack.length || delta === 0) return
      at = next
      subs.emit(read(), 'pop')
    },
    href: (to) => normalize(to),
    subscribe: (fn) => subs.add(fn),
  }
}
