/**
 * A router.
 *
 *     const App = createRouter({
 *       routes: [
 *         { path: '/', component: Home },
 *         { path: '/7guis', component: Guis, children: [{ path: ':task', component: Task }] },
 *       ],
 *       fallback: NotFound,
 *     })
 *     render(App(), document.getElementById('root')!)
 *
 * Routes are a data structure rather than a tree of `<Route>` elements, because
 * `component()` builds descriptors with an empty children array and so a
 * component cannot take positional children at all. That constraint turns out
 * to be a good one: the route table is a plain value, so it can be shared with
 * a nav bar, a sitemap, or a server, none of which want to render anything.
 *
 * Route state is published as signals through one context token. Context here
 * is not reactive, and a component that reads no props is never woken by its
 * parent, so passing params down as props would leave anything below the route
 * component stale. Reading `useParams(ctx).value` inside a render thunk is an
 * ordinary tracked read and updates whatever actually depends on it, however
 * deep it sits.
 *
 * This module imports the element factory, signals and context directly rather
 * than through `../index.ts`. Going through the barrel would pull the whole
 * reconciler, the DOM host, portals and the resource helpers into every bundle
 * that imports a router — `src/index.optional.test.ts` holds that line. Note
 * what is missing as a result: `render()` is the application's call to make,
 * not the router's, so nothing here depends on the reconciler at all.
 */

import { asyncComponent, component, createElement } from 'visage-dom/element'
import { context } from 'visage-dom/context'
import { computed, flushSync, signal } from 'visage-dom/signal'
import type { ReadonlySignal } from 'visage-dom/signal'
import type { Child, ComponentFn, Ctx } from 'visage-dom/types'
import { browserHistory } from './history/index.ts'
import type { History, NavigationAction } from './history/index.ts'
import { buildPath, flattenRoutes, matchRoutes, parsePath, resolvePath } from './match/index.ts'
import type { Branch, Matched } from './match/index.ts'

export {
  browserHistory,
  hashHistory,
  memoryHistory,
  type History,
  type HistoryOptions,
  type NavigationAction,
  type NavigationListener,
} from './history/index.ts'
export {
  buildPath,
  flattenRoutes,
  joinPaths,
  matchBranch,
  matchRoutes,
  parsePath,
  resolvePath,
  type Branch,
  type Matched,
  type MatchableRoute,
  type PathParts,
} from './match/index.ts'

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface LoadArgs {
  readonly params: Readonly<Record<string, string>>
  readonly location: RouteLocation
  /** Aborted when a newer navigation supersedes this one, or on unmount. */
  readonly signal: AbortSignal
}

export interface LoaderState<T> {
  readonly status: 'pending' | 'ready' | 'error'
  readonly data?: T
  readonly error?: unknown
}

export interface RouteDef {
  /** Relative to the parent. `''` is an index route: exactly the parent path. */
  readonly path: string
  readonly component?: ComponentFn<{}>
  /** Loaded on first visit, then remembered. Rendered through an async component. */
  readonly lazy?: () => Promise<ComponentFn<{}> | { readonly default: ComponentFn<{}> }>
  /** Shown while `lazy` is in flight. */
  readonly pending?: Child
  /** Route data. Runs on navigation; read it back with `useLoader`. */
  readonly load?: (args: LoadArgs) => unknown
  readonly children?: readonly RouteDef[]
}

export interface RouteLocation {
  /** Pathname, search and hash together — what you would pass to `navigate`. */
  readonly path: string
  readonly pathname: string
  readonly search: string
  readonly hash: string
  readonly query: URLSearchParams
  readonly params: Readonly<Record<string, string>>
  /** The history entry this location belongs to. */
  readonly key: string
  readonly action: NavigationAction
}

export interface NavigateOptions {
  readonly replace?: boolean
  readonly state?: unknown
  /** Set false to leave the scroll position alone. */
  readonly scroll?: boolean
}

export interface RouterApi {
  readonly location: ReadonlySignal<RouteLocation>
  readonly params: ReadonlySignal<Readonly<Record<string, string>>>
  readonly matches: ReadonlySignal<readonly Matched<RouteDef>[]>
  /** One entry per matched depth. Depths with no `load` report `ready`. */
  readonly loaders: ReadonlySignal<readonly LoaderState<unknown>[]>
  /** A path, or a number to move through history the way `history.go` does. */
  navigate(to: string | number, options?: NavigateOptions): void
  /** Resolve a possibly-relative path against the current location. */
  resolve(to: string): string
  /** What `to` should look like in an `href` under the active history backend. */
  href(to: string): string
}

export interface RouterOptions {
  readonly routes: readonly RouteDef[]
  /** Defaults to `browserHistory()`. */
  readonly history?: History
  /** Rendered when nothing matches. Without one, the outlet renders nothing. */
  readonly fallback?: ComponentFn<{}>
  /** Manage scroll position across navigations. Defaults to true. */
  readonly scroll?: boolean
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

const RouterCtx = context<RouterApi>('router')

/**
 * The depth the *next* outlet should render. The router provides 0; each outlet
 * renders its own depth and provides one more. So inside a route component the
 * injected value is one past its own depth, which is what `ownDepth` undoes.
 */
const DepthCtx = context<number>('router.depth', 0)

function ownDepth(ctx: Ctx): number {
  return ctx.inject(DepthCtx) - 1
}

// ---------------------------------------------------------------------------
// Lazy routes
// ---------------------------------------------------------------------------

/**
 * Both maps are keyed on the route object, which is declared once and lives as
 * long as the table. Minting a fresh wrapper per render would give the outlet a
 * different component definition each time, and the reconciler would remount —
 * re-showing the pending view on every update.
 */
const wrappers = new WeakMap<RouteDef, ComponentFn<{}>>()
const loaded = new WeakMap<RouteDef, ComponentFn<{}>>()

function lazyWrapper(route: RouteDef): ComponentFn<{}> {
  const existing = wrappers.get(route)
  if (existing !== undefined) return existing

  const load = route.lazy
  const wrapper = asyncComponent(
    async function* () {
      yield route.pending ?? null
      const mod = await load?.()
      const view = mod === undefined ? undefined : typeof mod === 'function' ? mod : mod.default
      if (view === undefined) {
        throw new Error(
          `visage-router: the lazy import for "${route.path}" resolved to nothing. ` +
            'Return the component, or a module with it as the default export.',
        )
      }
      // Remembered, so the next visit renders it synchronously. An async
      // generator always costs at least a microtask, so this is the only way
      // for a revisit to avoid flashing the pending view again.
      loaded.set(route, view)
      yield () => view()
    },
    { name: 'LazyRoute' },
  )

  wrappers.set(route, wrapper)
  return wrapper
}

function viewFor(route: RouteDef): Child {
  if (route.component !== undefined) return route.component()
  const already = loaded.get(route)
  if (already !== undefined) return already()
  if (route.lazy !== undefined) return lazyWrapper(route)()
  // A layout route with no view of its own is legal — it exists only to group
  // children under a path prefix — so it renders straight through to them. The
  // recursion terminates because the outlet below it eventually runs off the
  // end of the match chain and renders nothing.
  return Outlet()
}

// ---------------------------------------------------------------------------
// Scroll
// ---------------------------------------------------------------------------

function canScroll(): boolean {
  return typeof globalThis.scrollTo === 'function'
}

function scrollToHash(hash: string): boolean {
  const id = hash.slice(1)
  if (id === '') return false
  const target = globalThis.document?.getElementById(decodeURIComponent(id))
  if (target === null || target === undefined) return false
  target.scrollIntoView()
  return true
}

// ---------------------------------------------------------------------------
// createRouter
// ---------------------------------------------------------------------------

export function createRouter(options: RouterOptions): ComponentFn<{}> {
  const history = options.history ?? browserHistory()
  const branches: Branch<RouteDef>[] = flattenRoutes(options.routes)
  const managesScroll = options.scroll !== false

  // Built once so a 404 keeps the same route identity across navigations, and
  // the fallback component is not remounted on every miss.
  const missed: readonly Matched<RouteDef>[] =
    options.fallback === undefined
      ? []
      : [{ route: { path: '*', component: options.fallback }, params: {}, pathname: '/' }]

  interface RouteState {
    readonly location: RouteLocation
    readonly matches: readonly Matched<RouteDef>[]
  }

  function toState(path: string, key: string, action: NavigationAction): RouteState {
    const { pathname, search, hash } = parsePath(path)
    const matched = matchRoutes(branches, pathname)
    const matches = matched ?? missed
    const params = matches[matches.length - 1]?.params ?? {}
    return {
      location: {
        path,
        pathname,
        search,
        hash,
        query: new URLSearchParams(search),
        params,
        key,
        action,
      },
      matches,
    }
  }

  return component(
    function* () {
      const state = signal<RouteState>(toState(history.current, history.key, 'pop'))
      const loaders = signal<readonly LoaderState<unknown>[]>([])

      const positions = new Map<string, readonly [number, number]>()
      let inflight: AbortController | null = null
      // Through a closure so the teardown below is not narrowed to `null`:
      // `inflight` is only ever assigned inside `runLoaders`, which flow
      // analysis cannot see from the `finally`.
      const abortInflight = (): void => inflight?.abort()

      // ---- loaders ----

      function runLoaders(next: RouteState): void {
        abortInflight()
        const controller = new AbortController()
        inflight = controller

        const results: LoaderState<unknown>[] = []
        const settle = (depth: number, value: LoaderState<unknown>): void => {
          if (controller.signal.aborted) return
          const copy = [...loaders.peek()]
          copy[depth] = value
          loaders.value = copy
        }

        next.matches.forEach((match, depth) => {
          const load = match.route.load
          if (load === undefined) {
            results[depth] = { status: 'ready' }
            return
          }
          const args: LoadArgs = {
            params: match.params,
            location: next.location,
            signal: controller.signal,
          }
          let value: unknown
          try {
            value = load(args)
          } catch (error) {
            results[depth] = { status: 'error', error }
            return
          }
          if (value instanceof Promise) {
            results[depth] = { status: 'pending' }
            void value.then(
              (data) => settle(depth, { status: 'ready', data }),
              (error) => settle(depth, { status: 'error', error }),
            )
            return
          }
          // A synchronous loader settles synchronously, so it never flashes a
          // pending state it was never going to be in.
          results[depth] = { status: 'ready', data: value }
        })

        loaders.value = results
      }

      // ---- scroll ----

      function savePosition(): void {
        if (!managesScroll || !canScroll()) return
        positions.set(history.key, [globalThis.scrollX, globalThis.scrollY])
      }

      function applyScroll(next: RouteState, action: NavigationAction): void {
        if (!managesScroll || !canScroll()) return
        // The DOM has to exist before a hash target can be found or a saved
        // position can mean anything, so commit first. This is the one place
        // the router leans on flushSync; proposal C3 (`ctx.onCommit`) would be
        // the proper seam.
        flushSync()

        if (action === 'pop') {
          const saved = positions.get(next.location.key)
          if (saved !== undefined) {
            globalThis.scrollTo(saved[0], saved[1])
            return
          }
        }
        // A replace usually means "same page, different query"; moving the
        // viewport under the reader would be wrong.
        if (action === 'replace') return

        if (scrollToHash(next.location.hash)) return
        globalThis.scrollTo(0, 0)
        if (next.location.hash !== '') {
          // Lazy routes and loaders mount their content after this tick, so the
          // anchor may not exist yet. Worth exactly one retry.
          globalThis.setTimeout(() => scrollToHash(next.location.hash), 0)
        }
      }

      // ---- navigation ----

      // Set by a `navigate({ scroll: false })` and consumed by the very next
      // notification, which is the one that call is about to cause.
      let skipScrollOnce = false

      function onNavigate(to: string, action: NavigationAction): void {
        const next = toState(to, history.key, action)
        state.value = next
        runLoaders(next)
        if (skipScrollOnce) {
          skipScrollOnce = false
          return
        }
        applyScroll(next, action)
      }

      const api: RouterApi = {
        location: computed(() => state.value.location),
        params: computed(() => state.value.location.params),
        matches: computed(() => state.value.matches),
        loaders,
        navigate(to, navOptions) {
          if (typeof to === 'number') {
            savePosition()
            history.go(to)
            return
          }
          const target = resolvePath(to, state.peek().location.path)
          savePosition()
          if (navOptions?.scroll === false) skipScrollOnce = true
          if (navOptions?.replace === true) history.replace(target, navOptions.state)
          else history.push(target, navOptions?.state)
        },
        resolve: (to) => resolvePath(to, state.peek().location.path),
        href: (to) => history.href(resolvePath(to, state.peek().location.path)),
      }

      // `try`/`finally` rather than `using` here: the teardown is three lines
      // and one of them is conditional, and Bun's bundler still lowers `using`
      // into `SuppressedError` helpers that would land in every router bundle.
      // The generator body stays open across the yield either way, so `finally`
      // runs when `gen.return()` unwinds this on unmount.
      const unsubscribe = history.subscribe(onNavigate)
      let restoreScroll: (() => void) | undefined

      try {
        if (managesScroll && canScroll()) {
          // Recorded continuously rather than on the way out, because `popstate`
          // fires after the URL has already changed — by then the position that
          // needed saving belonged to an entry we have left.
          globalThis.addEventListener(
            'scroll',
            () => positions.set(history.key, [globalThis.scrollX, globalThis.scrollY]),
            { passive: true, signal: this.aborted },
          )
          const previous = globalThis.history.scrollRestoration
          globalThis.history.scrollRestoration = 'manual'
          restoreScroll = () => {
            globalThis.history.scrollRestoration = previous
          }
        }

        this.provide(RouterCtx, api)
        this.provide(DepthCtx, 0)

        runLoaders(state.peek())

        yield () => Outlet()
      } finally {
        unsubscribe()
        abortInflight()
        restoreScroll?.()
      }
    },
    { name: 'Router' },
  )
}

// ---------------------------------------------------------------------------
// Outlet
// ---------------------------------------------------------------------------

/**
 * Where a route's children render.
 *
 * The router renders one implicitly at the top, so an outlet only has to be
 * written inside a route that has children of its own.
 */
export const Outlet: ComponentFn<{}> = component(
  function* () {
    const router = this.inject(RouterCtx)
    const depth = this.inject(DepthCtx)
    this.provide(DepthCtx, depth + 1)

    yield () => {
      const match = router.matches.value[depth]
      return match === undefined ? null : viewFor(match.route)
    }
  },
  { name: 'Outlet' },
)

// ---------------------------------------------------------------------------
// Links
// ---------------------------------------------------------------------------

export interface LinkProps {
  /** Absolute, or relative to the current location. */
  readonly href: string
  readonly replace?: boolean
  readonly state?: unknown
  readonly scroll?: boolean
  /** Only count as active on an exact match. Defaults to false. */
  readonly end?: boolean
  /** Added to `class` while active. Defaults to `'active'`. */
  readonly activeClass?: string
  readonly class?: string
  readonly title?: string
  readonly target?: string
  readonly download?: string
  readonly attrs?: Readonly<Record<string, string>>
  readonly onclick?: (event: MouseEvent) => void
  readonly children?: Child | Child[]
}

/** Anything with a scheme, or protocol-relative. Left to the browser. */
function isExternal(href: string): boolean {
  return /^[a-zA-Z][a-zA-Z\d+\-.]*:/.test(href) || href.startsWith('//')
}

function isActive(current: string, target: string, end: boolean): boolean {
  if (current === target) return true
  // The root is a prefix of everything, so prefix-matching it would light up
  // every link in the nav at once.
  if (end || target === '/') return false
  return current.startsWith(target.endsWith('/') ? target : `${target}/`)
}

function toChildren(children: Child | Child[] | undefined): readonly Child[] {
  if (children === undefined) return []
  return Array.isArray(children) ? children : [children]
}

/**
 * A link.
 *
 * It renders a real anchor with a real `href`, so middle-click, cmd-click,
 * "open in new tab" and "copy link address" all keep working; the click handler
 * only takes over for the plain left-click that would otherwise reload the page.
 */
export const A: ComponentFn<LinkProps> = component<LinkProps>(
  function* (props) {
    const router = this.inject(RouterCtx)

    // Handlers run outside a tracking window, so these reads see current values
    // and subscribe to nothing. There is no stale-closure problem to solve and
    // nothing to memoize.
    const onclick = (event: MouseEvent): void => {
      props.onclick?.(event)
      if (event.defaultPrevented) return
      if (event.button !== 0) return
      if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return
      if (props.download !== undefined) return
      const target = props.target
      if (target !== undefined && target !== '' && target !== '_self') return
      if (isExternal(props.href)) return

      event.preventDefault()
      router.navigate(props.href, {
        replace: props.replace === true,
        state: props.state,
        scroll: props.scroll !== false,
      })
    }

    yield () => {
      const external = isExternal(props.href)
      const resolved = external ? props.href : router.resolve(props.href)
      const active =
        !external &&
        isActive(router.location.value.pathname, parsePath(resolved).pathname, props.end === true)

      const classes = [props.class, active ? (props.activeClass ?? 'active') : undefined]
        .filter((one): one is string => one !== undefined && one !== '')
        .join(' ')

      const attrs: Record<string, string> = {
        ...props.attrs,
        'data-current': String(active),
      }
      if (active) attrs['aria-current'] = 'page'

      const attributes: Record<string, unknown> = {
        href: external ? props.href : router.href(props.href),
        onclick,
        attrs,
      }
      if (classes !== '') attributes['class'] = classes
      if (props.title !== undefined) attributes['title'] = props.title
      if (props.target !== undefined) attributes['target'] = props.target
      if (props.download !== undefined) attributes['download'] = props.download

      return createElement('a', attributes, toChildren(props.children))
    }
  },
  { name: 'A' },
)

// ---------------------------------------------------------------------------
// Reading route state
// ---------------------------------------------------------------------------

export function useRouter(ctx: Ctx): RouterApi {
  return ctx.inject(RouterCtx)
}

export function useLocation(ctx: Ctx): ReadonlySignal<RouteLocation> {
  return ctx.inject(RouterCtx).location
}

export function useParams(ctx: Ctx): ReadonlySignal<Readonly<Record<string, string>>> {
  return ctx.inject(RouterCtx).params
}

export function useNavigate(ctx: Ctx): RouterApi['navigate'] {
  return ctx.inject(RouterCtx).navigate
}

export type SearchParamsInit =
  | URLSearchParams
  | Readonly<Record<string, string | null | undefined>>
  | ((previous: URLSearchParams) => URLSearchParams)

export type SetSearchParams = (next: SearchParamsInit, options?: NavigateOptions) => void

/**
 * Query state, read as a signal and written as a navigation.
 *
 *     const [query, setQuery] = useSearchParams(ctx)
 *     yield () => button({ onclick: () => setQuery({ arm: 'fine' }) }, query.value.get('arm'))
 *
 * Writing pushes by default, the same as React Router — pass `{ replace: true }`
 * for a control that should not fill the back button with every intermediate
 * state.
 */
export function useSearchParams(
  ctx: Ctx,
): [ReadonlySignal<URLSearchParams>, SetSearchParams] {
  const router = ctx.inject(RouterCtx)
  const query = computed(() => router.location.value.query)

  const set: SetSearchParams = (next, navOptions) => {
    const current = router.location.peek()
    let out: URLSearchParams
    if (typeof next === 'function') out = next(new URLSearchParams(current.search))
    else if (next instanceof URLSearchParams) out = next
    else {
      out = new URLSearchParams(current.search)
      for (const [key, value] of Object.entries(next)) {
        if (value === null || value === undefined) out.delete(key)
        else out.set(key, value)
      }
    }
    const search = out.toString()
    router.navigate(
      `${current.pathname}${search === '' ? '' : `?${search}`}${current.hash}`,
      navOptions,
    )
  }

  return [query, set]
}

const NOT_LOADED: LoaderState<never> = { status: 'pending' }

/**
 * The data the nearest enclosing route's `load` produced.
 *
 * Depth comes from context rather than an argument, so a component several
 * levels inside a route still reads that route's data.
 */
export function useLoader<T>(ctx: Ctx): ReadonlySignal<LoaderState<T>> {
  const router = ctx.inject(RouterCtx)
  const depth = ownDepth(ctx)
  return computed(() => (router.loaders.value[depth] ?? NOT_LOADED) as LoaderState<T>)
}
