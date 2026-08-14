/**
 * Fiber tree, keyed reconciliation, and the generator driver.
 *
 * These three live together because they are mutually recursive: driving a
 * generator produces a view, reconciling a view mounts child components, and
 * mounting a component drives a generator.
 */

import { defaultOf, hasDefault, type Context } from '../context/index.ts'
import { DEV } from '../dev/flag.ts'
import { flatten, isComponentDef, isElement } from '../element/index.ts'
import { isPortal } from '../portal/index.ts'
import {
  closeTracking,
  isTracking,
  openTracking,
  resubscribe,
  scheduleUpdate,
  unsubscribeAll,
  untrack,
  type Source,
  type Subscriber,
} from '../signal/index.ts'
import type {
  Child,
  ComponentDef,
  ComponentGen,
  AsyncComponentGen,
  Ctx,
  ElementNode,
  Key,
  View,
  ViewFn,
  Yielded,
} from '../types/index.ts'
import type { Host, HostEl, HostNode } from '../host/index.ts'

// `renderWith`'s signature names all three, so a caller reading it should not
// have to chase a second import to use it. The seam itself lives in
// `../host/index.ts`, which is where a host implementation should reach.
export type { Host, HostEl, HostNode } from '../host/index.ts'

// ---------------------------------------------------------------------------
// Fibers
// ---------------------------------------------------------------------------

interface TextFiber {
  kind: 'text'
  key: Key | undefined
  dom: HostNode
  value: string
}

interface HoleFiber {
  kind: 'hole'
  key: Key | undefined
  dom: HostNode
}

interface ElFiber {
  kind: 'el'
  key: Key | undefined
  dom: HostEl
  tag: string
  children: Fiber[]
  /** What this element's `ref` handed back, run when the fiber is disposed. */
  detach: (() => void) | undefined
  /** The descriptor that produced this fiber — also where its props live (`vnode.props`), so patch's previous-props read has to happen before this is overwritten. */
  vnode: ElementNode
}

interface CompFiber {
  kind: 'comp'
  key: Key | undefined
  def: ComponentDef<object>
  inst: Instance
  child: Fiber
  /** The descriptor that produced this fiber, for the identity bail in `patch`. */
  vnode: ElementNode
}

/**
 * A component whose root view is an array.
 *
 * Arrays in a *child* slot are spliced away by `flatten()` before they ever
 * reach a fiber, so this only appears where a whole view is one: a component's
 * root, or a `render()` root. What that means in practice is that a fragment is
 * never a direct member of a child list, but a `comp` fiber sitting in a keyed
 * list may own several DOM nodes through one of these.
 */
interface FragFiber {
  kind: 'frag'
  key: Key | undefined
  children: Fiber[]
  /** The array that produced this fiber, for the identity bail in `patch`. */
  vnode: readonly Child[]
}

/**
 * Children rendered into an element outside this subtree.
 *
 * `dom` holds the portal's place among its logical siblings so they anchor
 * against something, while the children live in `target` ahead of `tail`. The
 * tail is what lets two portals share one target without interleaving: each
 * appends its own marker, and each inserts before its own.
 */
interface PortalFiber {
  kind: 'portal'
  key: Key | undefined
  dom: HostNode
  tail: HostNode
  target: HostEl
  children: Fiber[]
  vnode: ElementNode
}

export type Fiber =
  | TextFiber
  | HoleFiber
  | ElFiber
  | CompFiber
  | FragFiber
  | PortalFiber

/**
 * The first DOM node a fiber owns, which is what every anchor site wants.
 *
 * A component fiber has no node of its own; it borrows its child's. Making this
 * a lookup rather than a stored field removes a whole class of staleness bugs
 * when a component's root node changes type. A fragment holds at least one
 * child by construction, so this stays total.
 */
export function domOf(fiber: Fiber): HostNode {
  if (fiber.kind === 'comp') return domOf(fiber.child)
  if (fiber.kind === 'frag') return domOf(fiber.children[0] as Fiber)
  return fiber.dom
}

/** The last node a fiber owns. Only needed to anchor the end of a fragment. */
function lastDomOf(fiber: Fiber): HostNode {
  if (fiber.kind === 'comp') return lastDomOf(fiber.child)
  if (fiber.kind === 'frag') {
    return lastDomOf(fiber.children[fiber.children.length - 1] as Fiber)
  }
  return fiber.dom
}

/**
 * Move every node a fiber owns to sit before `anchor`.
 *
 * Inserting each of a fragment's nodes before the *same* anchor keeps them in
 * order, since each lands immediately after the one before it. Single-node
 * fibers, which is nearly all of them, take the direct path.
 */
function moveFiber(
  host: Host,
  fiber: Fiber,
  parent: HostEl,
  anchor: HostNode | null,
): void {
  if (fiber.kind === 'comp') {
    moveFiber(host, fiber.child, parent, anchor)
    return
  }
  if (fiber.kind === 'frag') {
    for (let i = 0; i < fiber.children.length; i++) {
      moveFiber(host, fiber.children[i] as Fiber, parent, anchor)
    }
    return
  }
  host.insert(parent, fiber.dom, anchor)
}

/**
 * Detach every node a fiber owns *in its own parent*, leaving disposal to the
 * caller. A portal's foreign content is not covered here; `disposeTree` handles
 * that, since it is the only pass that reaches the whole subtree.
 */
function removeDom(host: Host, fiber: Fiber): void {
  if (fiber.kind === 'comp') {
    removeDom(host, fiber.child)
    return
  }
  if (fiber.kind === 'frag') {
    for (let i = 0; i < fiber.children.length; i++) {
      removeDom(host, fiber.children[i] as Fiber)
    }
    return
  }
  host.remove(fiber.dom)
}

function keyOf(vnode: Child): Key | undefined {
  return isElement(vnode) ? vnode.key : undefined
}

function sameType(fiber: Fiber, vnode: Child): boolean {
  if (vnode === null || vnode === undefined || typeof vnode === 'boolean') {
    return fiber.kind === 'hole'
  }
  if (typeof vnode === 'string' || typeof vnode === 'number') {
    return fiber.kind === 'text'
  }
  // Before the isElement check: an array is an object, so without this it would
  // fall through to the final `return false` and never match its own fiber.
  if (Array.isArray(vnode)) {
    return fiber.kind === 'frag'
  }
  if (isElement(vnode)) {
    if (isComponentDef(vnode.tag)) {
      return fiber.kind === 'comp' && fiber.def === vnode.tag
    }
    if (isPortal(vnode.tag)) return fiber.kind === 'portal'
    return fiber.kind === 'el' && fiber.tag === vnode.tag
  }
  return false
}

/**
 * A fragment always holds at least one child, so `domOf` and `lastDomOf` stay
 * total. An empty view becomes a single hole, which costs one comment node only
 * for fragments that are actually empty.
 */
const EMPTY_FRAGMENT: readonly Child[] = [null]

function fragmentChildren(vnode: readonly Child[]): readonly Child[] {
  const kids = flatten(vnode)
  return kids.length === 0 ? EMPTY_FRAGMENT : kids
}

// ---------------------------------------------------------------------------
// Component instances — the generator driver
// ---------------------------------------------------------------------------

const NO_VIEW = Symbol('no-view')

/** Consecutive non-awaiting yields from an async component before we bail out. */
const SPIN_LIMIT = 1000

/**
 * `gen.return()` calls a teardown may take before we stop asking.
 *
 * A `yield` inside a `finally` keeps the generator alive through one `return()`,
 * so unmount has to ask more than once — but a loop around such a yield would
 * ask forever.
 */
const TEARDOWN_LIMIT = 100

/**
 * The component whose generator is currently being driven.
 *
 * Saved and restored around a resume exactly the way `openTracking` handles the
 * dependency window, and for the same reason: an `Instance` created while this
 * is set is, by construction, a child of it. That is the whole parent link —
 * fibers carry no upward pointer, and building one just for `inject` to walk
 * would cost every mount.
 *
 * The window has to span the commit as well as the resume, because children are
 * mounted from `#commit`, not from `gen.next()`.
 */
let currentInstance: Instance | null = null

// ---------------------------------------------------------------------------
// Error escalation
// ---------------------------------------------------------------------------

interface Failure {
  /** Where the walk begins, inclusive. Null means nothing can catch this. */
  start: Instance | null
  error: unknown
}

/**
 * Depth of in-flight mount/patch work, and the failures waiting for it to end.
 *
 * Escalation cannot happen the moment a component throws. Handing the error to
 * an ancestor resumes its generator, which commits a new view — and if an outer
 * `patch` is still on the stack, that outer call returns afterwards and
 * overwrites the fiber the ancestor just replaced, leaving the fallback
 * orphaned and the broken subtree installed. So failures queue here and drain
 * once the reconciler is idle.
 */
let reconcileDepth = 0
let pendingFailures: Failure[] | null = null

/**
 * Hosts touched by the reconcile in flight, for the end-of-commit notification.
 *
 * Allocated only when a host actually wants one — the DOM host leaves `commit`
 * off entirely, so the default path never builds this set. A host that paints
 * from its tree gets exactly one call per settled reconcile no matter how many
 * components committed inside it, which is what turns N generator resumes into
 * one repaint.
 */
let touchedHosts: Set<Host> | null = null

function enterReconcile(host: Host): void {
  reconcileDepth += 1
  if (host.commit !== undefined) (touchedHosts ??= new Set()).add(host)
}

function leaveReconcile(): void {
  reconcileDepth -= 1
  if (reconcileDepth !== 0) return

  if (touchedHosts !== null) {
    const hosts = touchedHosts
    touchedHosts = null
    for (const host of hosts) host.commit?.()
  }

  if (pendingFailures === null) return
  const failures = pendingFailures
  pendingFailures = null
  for (const failure of failures) escalate(failure.start, failure.error)
}

function queueFailure(start: Instance | null, error: unknown): void {
  ;(pendingFailures ??= []).push({ start, error })
}

/** `handleError`'s answer for "caught it, stop walking". */
const HANDLED: unique symbol = Symbol('view.handled')

/**
 * Offer the error to each ancestor in turn until one catches it.
 *
 * A generator that catches and then throws something else replaces the error
 * for the rest of the walk, so wrapping works the way it does in ordinary
 * `try`/`catch`.
 *
 * Nothing caught it means the tree has no boundary for this failure, and the
 * throw is rethrown rather than reported asynchronously — `render()` should
 * fail loudly, and a stack that still points at the cause is worth far more
 * than one from a microtask. Errors raised by the scheduler are caught per
 * subscriber in `flush()`, so this does not wedge a batch.
 */
function escalate(start: Instance | null, error: unknown): void {
  let node = start
  let current = error
  while (node !== null) {
    // Read the parent first: handling may finish and tear down this instance.
    const next = node.parentComponent
    const outcome = node.handleError(current)
    if (outcome === HANDLED) return
    current = outcome
    node = next
  }
  throw current
}

class Instance implements Subscriber {
  disposed = false
  depth: number

  #gen: ComponentGen | AsyncComponentGen | null = null
  #def: ComponentDef<object>
  #props: object
  #deps = new Set<Source>()
  /**
   * The other half of a double buffer. A sync resume rebuilds its dependency
   * set from scratch every time, so the two sets are swapped and cleared rather
   * than allocating a fresh one per resume — at a thousand rows that is a
   * thousand allocations per update.
   */
  #depsAlt = new Set<Source>()
  /** Allocated on demand: most components never read `this.aborted`. */
  #abort: AbortController | null = null
  #finished = false
  #fiber: CompFiber | null = null
  #host: Host
  #parent: HostEl
  /** Async only: a resume loop is running, and the props waiting behind it. */
  #looping = false
  #pending: object | typeof NO_VIEW = NO_VIEW
  /**
   * Deps collected during the current resume. `this.track()` adds into this so
   * that reads after an `await` survive the end-of-resume resubscribe.
   */
  #resumeDeps: Set<Source> | null = null
  /** Consecutive auto-advances that completed without ever awaiting. */
  #spin = 0
  /** Nearest enclosing component, captured at construction. See `currentInstance`. */
  #parentInstance: Instance | null
  /** Context values provided by this component. Most provide none. */
  #contexts: Map<symbol, unknown> | null = null

  /**
   * The live `props` object handed to the generator — a stable Proxy reading
   * through to `#props`. Allocated on first use like everything else here;
   * components that never touch their props never pay for it.
   */
  #propsView: object | null = null
  /**
   * Prop keys read during the resume that produced the current view, and the
   * other half of the double buffer. Same trick as `#deps`/`#depsAlt`: a resume
   * rebuilds the set from scratch, so the two are swapped rather than
   * reallocated.
   */
  #propKeys: Set<string> | null = null
  #propKeysAlt: Set<string> | null = null
  /** Collects prop reads for the resume in flight. Null outside a resume. */
  #resumePropKeys: Set<string> | null = null
  /**
   * True only while the generator *function* is being called, before its body
   * has begun. A prop read in that window can only have come from destructuring
   * in the parameter list, which reads once and never again. See `#readProp`.
   */
  #constructing = false
  /** Dev only: a prop was read on some resume, so a later empty set is news. */
  #everReadProps = false
  #warnedStaleProps = false
  #warnedInertThunk = false

  /**
   * The parked render thunk, or null while the generator is still driving.
   *
   * Non-null means the generator is never resumed for an update again: it sits
   * suspended at the `yield` that produced this, which is exactly what keeps
   * `try`/`finally`, `using`, `yield*` teardown and `gen.throw()` working the
   * way they do for a looping component. Updates call the thunk instead.
   */
  #thunk: ViewFn | null = null
  /**
   * A render threw and the escalation walk has not decided this component's
   * fate yet. Only a parked component can be in this state — a looping one is
   * `#finished` by the throw, since the exception unwound its generator.
   */
  #broken = false
  /**
   * This component's generator is part-way through `handleError`.
   *
   * Recovery commits its fallback, and a fallback that throws from *inside*
   * `patch` is queued by `#commit` against this same instance. Because that
   * commit opened and closed its own reconcile frame, `leaveReconcile` drains
   * the queue immediately and hands the failure straight back here — an
   * unbounded synchronous loop that leaks the subtree it built on every turn.
   * Seeing the flag set means the error came out of our own recovery, so the
   * parent gets it instead.
   */
  #handling = false
  /** A resume was requested while one was already running. See `run`. */
  #needsResume = false

  readonly ctx: Ctx

  constructor(
    def: ComponentDef<object>,
    props: object,
    host: Host,
    parent: HostEl,
    depth: number,
  ) {
    this.#def = def
    this.#props = props
    this.#host = host
    this.#parent = parent
    this.depth = depth
    this.#parentInstance = currentInstance
    this.ctx = new InstanceCtx(this)
  }

  // --- Live props ------------------------------------------------------------

  /**
   * Record a prop read against the resume in flight.
   *
   * This is the props half of "everything read between resume and yield is a
   * dependency of that yield". Reads outside a resume — an event handler firing
   * later, say — record nothing and simply see the current value, which is the
   * same rule `record()` follows for signals, and which is why a handler that
   * reads `props.x` can never close over a stale one.
   */
  #readProp(key: string): void {
    if (DEV && this.#constructing) {
      // Parameter destructuring runs when the generator function is *called*,
      // before the body starts, so this branch is reachable from nothing else.
      throw new Error(
        `view: <${this.#def.name}> destructured its props in the parameter list. ` +
          'Props are live — destructuring reads them once, here, and the component ' +
          `would render the values from mount forever. Take \`props\` whole and read ` +
          '`props.' +
          key +
          '` inside the loop instead.',
      )
    }
    // The same condition a signal read obeys, so `untrack(() => props.x)` and a
    // read from an event handler behave identically for both.
    if (isTracking()) this.#resumePropKeys?.add(key)
  }

  /**
   * The object the generator sees. One Proxy per instance, stable for its whole
   * life, reading through to whatever `#props` currently holds. That stability
   * is the point: the generator is handed this object once, as its parameter,
   * and every later read through it is current — which is why nothing needs to
   * be sent back in at `yield`.
   *
   * The target is a fresh `{}` rather than the props object: the props object
   * may be the frozen `NO_PROPS` singleton, and a frozen target constrains what
   * the traps are allowed to return.
   */
  get propsView(): object {
    if (this.#propsView !== null) return this.#propsView
    // The target has no own properties, same reasoning as before: the props
    // object may be the frozen `NO_PROPS` singleton, and a frozen target
    // constrains what the traps are allowed to return. It still has to be
    // fresh per instance — `PROPS_VIEW_HANDLER` is what is shared, not this.
    const target = {}
    instanceForTarget.set(target, this)
    this.#propsView = new Proxy(target, PROPS_VIEW_HANDLER)
    return this.#propsView
  }

  /** For `PROPS_VIEW_HANDLER` — records a prop read against the resume in flight. */
  notePropRead(key: string): void {
    this.#readProp(key)
  }

  /** For `PROPS_VIEW_HANDLER`. */
  get liveProps(): object {
    return this.#props
  }

  /** For `PROPS_VIEW_HANDLER`'s DEV-mode `set` throw. */
  get displayName(): string {
    return this.#def.name
  }

  /** Open the prop-read window for a resume. Pairs with `#endPropWindow`. */
  #beginPropWindow(): void {
    const next = this.#propKeysAlt ?? new Set<string>()
    next.clear()
    this.#resumePropKeys = next
  }

  #endPropWindow(): void {
    const read = this.#resumePropKeys
    this.#resumePropKeys = null
    if (read === null) return
    this.#propKeysAlt = this.#propKeys
    this.#propKeys = read

    if (DEV) {
      if (read.size > 0) {
        this.#everReadProps = true
      } else if (this.#everReadProps && !this.#warnedStaleProps) {
        this.#warnedStaleProps = true
        console.warn(
          `view: <${this.#def.name}> read its props on an earlier render but not on ` +
            'this one. If it copied them into a local — `const { label } = props` — ' +
            'that local is now stale and will never update. Read `props.label` inside ' +
            'the render, or wrap a deliberate one-time read in `untrack()`.',
        )
      }
    }
  }

  // --- Render thunk diagnostics. Dev only; folded away in the bundle build. ---

  /**
   * A render thunk must return a view, synchronously.
   *
   * Neither mistake is caught by the type system in the place a reader would
   * look — an inferred `TYield` that does not fit `Yielded` is reported against
   * the whole `component()` call — and neither is visible at runtime without
   * this: a promise and a function both reach `mount()` as objects with no
   * `tag`, and build an `<undefined>` element rather than failing.
   */
  #checkThunkView(view: unknown): void {
    if (isThenable(view)) {
      throw new Error(
        `view: <${this.#def.name}> yielded an async render thunk. The thunk is ` +
          'called inside a synchronous tracking window, so it cannot await: ' +
          'reads after an await would subscribe to nothing. Await in the ' +
          'generator and park afterwards — `yield loading; const x = await …; ' +
          'yield () => view(x)` — or hold the result in a signal.',
      )
    }
    if (typeof view === 'function') {
      throw new Error(
        `view: <${this.#def.name}> yielded a render thunk that returned another ` +
          'function. The thunk returns the view itself — `yield () => div(…)`.',
      )
    }
  }

  /**
   * A component that read something during setup and nothing inside its thunk.
   *
   * It rendered correctly once and can never be woken again, so this is the
   * signal half of the stale-local mistake `#endPropWindow` warns about. It has
   * to fire here, at the moment of parking: a component in this state never
   * renders a second time, so a warning raised on any later render is a warning
   * never raised at all.
   */
  #warnInertThunk(setupRead: boolean, deps: Set<Source>): void {
    if (!setupRead || this.#warnedInertThunk) return
    if (deps.size > 0 || (this.#resumePropKeys?.size ?? 0) > 0) return
    this.#warnedInertThunk = true
    console.warn(
      `view: <${this.#def.name}> read a signal or a prop above its ` +
        '`yield () => …`, but its render thunk reads nothing, so nothing can ' +
        'ever wake it. Read inside the thunk — `n.value`, `props.label` — or ' +
        'wrap a deliberate one-time read in `untrack()`.',
    )
  }

  // --- Backing for InstanceCtx. Module-private, not part of the public API. ---

  get parentComponent(): Instance | null {
    return this.#parentInstance
  }

  /**
   * Offer an error raised somewhere below to this component's generator.
   *
   * `gen.throw` resumes it **at the `yield` it is suspended on**, so an ordinary
   * `try`/`catch` wrapped around that yield is the boundary. Nothing else is
   * needed: no boundary component, no lifecycle hook.
   *
   * Returns `HANDLED`, or the error the walk should carry upward — which is a
   * different one when the generator caught and threw something else. A
   * generator that does not catch is finished by the throw, its `finally`
   * blocks having already run, which is what a stack unwind looks like from
   * the outside.
   */
  handleError(error: unknown): unknown | typeof HANDLED {
    if (
      this.disposed ||
      this.#finished ||
      this.#gen === null ||
      this.#fiber === null ||
      this.#handling
    ) {
      return error
    }
    // An async generator's throw() returns a promise, so it cannot be resumed
    // synchronously here. Pass the error through untouched rather than tearing
    // the component down; its own try/catch around `await` still works.
    if (this.isAsync) return error

    this.#handling = true
    try {
      const gen = this.#gen as ComponentGen
      const deps = this.#depsAlt
      deps.clear()
      const prev = openTracking(deps)
      this.#resumeDeps = deps
      this.#beginPropWindow()
      let view: View | typeof NO_VIEW
      try {
        const result = gen.throw(error)
        // A catch may yield either a plain view or a new thunk; `#adopt` handles
        // both, so a parked boundary can park again on its fallback, and one
        // that yields an ordinary view goes back to being generator-driven.
        view = result.done ? NO_VIEW : this.#adopt(result.value, deps)
      } catch (rethrown) {
        // Either it did not catch, or it caught and threw something else. All of
        // them end this generator; the walk continues with whatever came out.
        this.#finished = true
        return rethrown
      } finally {
        closeTracking(prev)
        this.#resumeDeps = null
        this.#endPropWindow()
      }

      resubscribe(this, this.#deps, deps)
      this.#depsAlt = this.#deps
      this.#deps = deps

      if (view === NO_VIEW) {
        this.#finished = true
        unsubscribeAll(this, this.#deps)
        if (DEV && this.#thunk !== null) {
          console.warn(
            `view: <${this.#def.name}> caught an error and then finished. Its view ` +
              'is frozen at the last commit and the error keeps escalating. Yield a ' +
              'fallback from the catch — `catch { yield () => div("failed") }` — or ' +
              'wrap the whole thing in `while (true)` for the retry a loop gives you.',
          )
        }
        return error
      }

      this.#broken = false
      // A throw from here re-enters through `leaveReconcile`, and `#handling`
      // is what turns that into an escalation to the parent instead of a loop.
      this.#commit(view)
      return HANDLED
    } finally {
      this.#handling = false
    }
  }

  /**
   * A render threw. Decide who gets first refusal.
   *
   * A looping component builds its view *inside* `gen.next()`, so a throw there
   * unwinds its own generator and only an ancestor can catch it. A parked one
   * builds its view in a thunk the runtime calls — but the generator is alive
   * and suspended at the `yield` that produced that thunk, so the `try`
   * lexically wrapping that yield is still the nearest enclosing one, and
   * offering the error there is what keeps the two forms behaving alike. It is
   * also the policy `#commit` already follows for a throw from the subtree.
   */
  #fail(error: unknown): void {
    if (this.#thunk !== null) {
      this.#broken = true
      queueFailure(this, error)
      return
    }
    this.#finished = true
    // The generator is gone, so no signal can ever resume it. Matching the
    // NO_VIEW path in `#resumeSync`, which unsubscribes for the same reason —
    // without this a component killed by a render throw stays in every source's
    // subscriber set until something gets around to unmounting it.
    unsubscribeAll(this, this.#deps)
    queueFailure(this.#parentInstance, error)
  }

  provideContext<T>(key: Context<T>, value: T): void {
    ;(this.#contexts ??= new Map()).set(key.id, value)
  }

  /**
   * Nearest provider wins. A `#private` field is reachable on any instance of
   * the same class, so the walk needs no accessor and no public surface.
   */
  injectContext<T>(key: Context<T>): T {
    for (let node: Instance | null = this; node !== null; node = node.#parentInstance) {
      const provided = node.#contexts
      if (provided !== null && provided.has(key.id)) return provided.get(key.id) as T
    }
    if (hasDefault(key)) return defaultOf(key)
    throw new Error(
      `view: no value provided for context "${key.name}". Call this.provide(${key.name}, …) ` +
        'in an ancestor, or give the token a default.',
    )
  }

  get abortSignal(): AbortSignal {
    // `dispose` aborts the controller, but only when something had already read
    // this — allocation is lazy so most components never build one. Without the
    // check, a first read *after* teardown hands back a fresh, un-aborted
    // signal, and the canonical `if (this.aborted.aborted) return` guard after
    // an `await` reads false on a component that is already gone.
    if (this.#abort === null && this.disposed) return AbortSignal.abort()
    return (this.#abort ??= new AbortController()).signal
  }

  trackIn<T>(fn: () => T): T {
    // `unsubscribeAll` has already emptied the standing dep set, and the
    // `finally` below re-adds this instance to every source `fn` reads — which
    // would pin a dead component in `Signal.subs` for the life of the signal.
    // Nothing can resume it, so nothing is worth recording.
    if (this.disposed) return untrack(fn)
    // Collect into the in-flight resume's dep set when there is one, so the
    // resubscribe at the end of the resume keeps these rather than dropping
    // them; otherwise fall back to the instance's standing deps.
    const target = this.#resumeDeps ?? this.#deps
    const prev = openTracking(target)
    // Props are tracked the same way, so `this.track(() => props.x)` after an
    // `await` records the prop key just as it records a signal read. Falls back
    // to the standing set for the same reason the deps do.
    const prevKeys = this.#resumePropKeys
    this.#resumePropKeys = prevKeys ?? (this.#propKeys ??= new Set())
    try {
      return fn()
    } finally {
      closeTracking(prev)
      this.#resumePropKeys = prevKeys
      for (const dep of target) dep.subs.add(this)
    }
  }

  get isAsync(): boolean {
    return this.#def.isAsync
  }

  attach(fiber: CompFiber): void {
    this.#fiber = fiber
  }

  /**
   * Classify what came out of a `yield`, inside the caller's tracking window.
   *
   * A function is a render thunk: the generator parks at this yield and the
   * thunk is called now, and again on every later update, without the
   * generator being touched. `typeof` is enough to tell the two apart and
   * always will be — an `ElementNode` is a branded plain object and every other
   * `View` is a primitive or an array, so nothing a view can be is a function.
   *
   * Anything else is an ordinary view, and clears any park. That is how a
   * `catch` yielding a plain fallback returns a parked component to loop mode.
   */
  #adopt(yielded: Yielded, deps: Set<Source>): View {
    if (typeof yielded !== 'function') {
      this.#thunk = null
      return yielded
    }
    this.#thunk = yielded

    // Everything above the yield is setup: it runs once, so a dependency on it
    // could only ever schedule a render that produces the same view. Start the
    // window over, so the thunk's reads — and only those — are the deps.
    const setupRead =
      DEV && (deps.size > 0 || (this.#resumePropKeys?.size ?? 0) > 0)
    deps.clear()
    this.#resumePropKeys?.clear()

    const view = this.#callThunk()
    if (DEV) this.#warnInertThunk(setupRead, deps)
    return view
  }

  /** Call the parked thunk, checking in dev that it returned a view. */
  #callThunk(): View {
    const view = (this.#thunk as ViewFn)()
    if (DEV) this.#checkThunkView(view)
    return view
  }

  /**
   * Drive the generator one step, adopting a thunk if that is what comes out.
   *
   * Nothing is sent in. `props` is live, so the generator is already holding
   * the current object; there is nothing to hand back.
   */
  #advance(deps: Set<Source>): View | typeof NO_VIEW {
    const result = (this.#gen as ComponentGen).next()
    if (result.done) return NO_VIEW
    return this.#adopt(result.value, deps)
  }

  /**
   * Produce this component's next view with the tracking window open.
   *
   * This is the heart of the library: everything read between here and the
   * view coming back is recorded as a dependency of that view. Two sources,
   * one window — a generator that has not parked is resumed, and a parked one
   * has its thunk called.
   */
  #resumeSync(): View | typeof NO_VIEW {
    const deps = this.#depsAlt
    deps.clear()
    const prev = openTracking(deps)
    // So that a `this.track()` inside this window collects into the set the
    // resubscribe below keeps, rather than into the outgoing one.
    this.#resumeDeps = deps
    this.#beginPropWindow()
    let view: View | typeof NO_VIEW
    try {
      view = this.#thunk === null ? this.#advance(deps) : this.#callThunk()
    } finally {
      closeTracking(prev)
      this.#resumeDeps = null
      this.#endPropWindow()
    }
    resubscribe(this, this.#deps, deps)
    this.#depsAlt = this.#deps
    this.#deps = deps

    if (view === NO_VIEW) {
      this.#finished = true
      unsubscribeAll(this, this.#deps)
      return NO_VIEW
    }
    return view
  }

  /**
   * Async resume. The synchronous tracking window cannot span an `await`, so
   * only reads before the first await are captured automatically; use
   * `this.track()` for reads after one. See README "Async caveat".
   *
   * Also reports whether the generator truly awaited anything: if its promise
   * settled before a macrotask boundary, it did not, which is how the spin
   * guard tells a real async component from an accidental infinite loop.
   */
  async #resumeAsync(): Promise<{ view: View | typeof NO_VIEW; awaited: boolean }> {
    const gen = this.#gen as AsyncComponentGen
    const deps = new Set<Source>()
    this.#resumeDeps = deps
    const prev = openTracking(deps)
    this.#beginPropWindow()
    let promise: Promise<IteratorResult<Yielded, void>>
    try {
      promise = gen.next()
    } finally {
      closeTracking(prev)
      // Closed with the synchronous window: a prop read after an `await` is
      // outside it, exactly as a signal read there is.
      this.#endPropWindow()
    }

    let awaited = false
    const timer = setTimeout(() => {
      awaited = true
    }, 0)
    let result: IteratorResult<Yielded, void>
    try {
      result = await promise
    } finally {
      clearTimeout(timer)
      this.#resumeDeps = null
    }

    // Unmounted while we were awaiting. Everything below is worse than useless
    // now: `#adopt` would call a render thunk after the component's `using`
    // scopes had already unwound, and `resubscribe` would re-add this disposed
    // instance to every source read before the await — `unsubscribeAll` has
    // been and gone, so nothing would ever take it out again.
    if (this.disposed) return { view: NO_VIEW, awaited }

    let view: View | typeof NO_VIEW = NO_VIEW
    if (!result.done) {
      if (typeof result.value === 'function') {
        // The synchronous window closed at the first `await`, so open a fresh
        // one around the thunk. This is the one place the async caveat does not
        // apply: from here the component renders synchronously, and everything
        // the thunk reads is tracked without `this.track()`.
        const inner = openTracking(deps)
        this.#beginPropWindow()
        try {
          view = this.#adopt(result.value, deps)
        } finally {
          closeTracking(inner)
          this.#endPropWindow()
        }
      } else {
        view = this.#adopt(result.value, deps)
      }
    }

    // `deps` now holds the synchronous window plus anything this.track() added.
    resubscribe(this, this.#deps, deps)
    this.#deps = deps

    if (view === NO_VIEW) {
      this.#finished = true
      unsubscribeAll(this, this.#deps)
      return { view: NO_VIEW, awaited }
    }
    return { view, awaited }
  }

  /**
   * Drive an async component. Unlike a sync component — which suspends at each
   * yield until something resumes it — an async component keeps advancing, so
   * that a prologue like `yield loading; await fetch(); yield data` completes on
   * its own. A generator that yields forever without awaiting would spin, so
   * that case is detected and reported instead of hanging the page.
   */
  async #loop(): Promise<void> {
    if (this.#looping) return
    this.#looping = true
    try {
      while (!this.disposed && !this.#finished) {
        const { view, awaited } = await this.#resumeAsync()
        if (this.disposed || !this.#fiber) return
        if (view === NO_VIEW) return

        this.#commit(view)

        if (this.#thunk !== null) {
          // Parked. Resuming again would run the code past that yield, and
          // there is nothing left to drive: updates arrive through `run()`,
          // which takes the sync path from here.
          if (this.#pending !== NO_VIEW) {
            this.#props = this.#pending as object
            this.#pending = NO_VIEW
            this.run()
          }
          return
        }

        this.#spin = awaited ? 0 : this.#spin + 1
        if (this.#spin > SPIN_LIMIT) {
          this.#spin = 0
          throw new Error(
            `view: async component <${this.#def.name}> yielded ${SPIN_LIMIT} times ` +
              'without awaiting anything. Await something in the loop, park on a ' +
              'render thunk — `yield () => view` — or make it a sync component so ' +
              'it suspends at each yield.',
          )
        }

        if (this.#pending !== NO_VIEW) {
          this.#props = this.#pending as object
          this.#pending = NO_VIEW
        }
        // Yield to the macrotask queue so the page stays responsive.
        await new Promise((resolve) => setTimeout(resolve, 0))
      }
    } finally {
      this.#looping = false
    }
  }

  /**
   * Call the generator function.
   *
   * Parameters are bound when a generator function is *called*, not at the
   * first `next()`, so any prop read that happens in here came from
   * destructuring in the parameter list. `#constructing` is what lets
   * `#readProp` tell that apart from an ordinary read and refuse it.
   *
   * `this` is bound at the same moment and by the same rule, which is the whole
   * reason the context arrives that way rather than as a second parameter: the
   * binding belongs to the generator's execution context, so it survives every
   * `yield` and every `await` in the body, and any arrow the body creates closes
   * over it. A module-global would not — it is only correct inside a resume.
   */
  #construct(): ComponentGen | AsyncComponentGen {
    this.#constructing = true
    try {
      return this.#def.render.call(this.ctx, this.propsView)
    } finally {
      this.#constructing = false
    }
  }

  /** First render. Returns the initial child fiber. */
  mount(before: HostNode | null, into: HostEl = this.#parent): Fiber {
    const host = this.#host
    const outer = currentInstance
    currentInstance = this
    enterReconcile(host)
    try {
      if (!this.isAsync) {
        try {
          this.#gen = this.#construct() as ComponentGen
          const view = this.#resumeSync()
          // Mounting the view belongs inside the try: a `ref`, a prop, or the
          // DEV child guard can throw from in here, and leaving it outside sent
          // that error straight past `#fail` and out of `render()` — the same
          // failure a boundary catches happily on a later patch.
          return mount(
            host,
            view === NO_VIEW ? null : view,
            this.#parent,
            before,
            this.depth + 1,
            into,
          )
        } catch (error) {
          // Nothing usable was produced. Put a hole in its place so the
          // enclosing patch finishes against a well-formed tree, and let the
          // walk decide what to render once things are idle — starting at this
          // component if it parked before throwing, and at its parent otherwise.
          this.#fail(error)
          return mount(host, null, this.#parent, before, this.depth + 1, into)
        }
      }

      this.#gen = this.#construct() as AsyncComponentGen
      // Async: place a hole now and fill it as values arrive.
      const placeholder = mount(host, null, this.#parent, before, this.depth + 1, into)
      void this.#loop().catch(reportError)
      return placeholder
    } finally {
      currentInstance = outer
      leaveReconcile()
    }
  }

  /**
   * Re-render with new props from a parent.
   *
   * A parent that re-renders hands every child a freshly built descriptor, but
   * most of those children were handed the same values as last time, and most
   * of the rest changed a key this child never looks at. So the question asked
   * here is not "are these props equal" but "did anything this component
   * actually read change" — the same question the signal layer asks, over the
   * set `#propKeys` collected at the last yield.
   *
   * A component that read no props is never resumed by its parent at all. It
   * stays live regardless: a signal wakes whatever read it.
   */
  update(props: object): void {
    const prev = this.#props
    this.#props = props
    if (prev === props) return

    // `memo: false` still means what it always did: resume on every parent
    // render. Live props narrow *which* props matter, but they cannot see state
    // the props do not describe at all — nor a prop object mutated in place,
    // since the value compared is the same reference either way.
    if (!this.#def.memo) {
      this.run()
      return
    }

    const read = this.#propKeys
    if (read === null || read.size === 0) return

    const before = prev as Record<string, unknown>
    const after = props as Record<string, unknown>
    for (const key of read) {
      if (!Object.is(before[key], after[key])) {
        this.run()
        return
      }
    }
  }

  /** Subscriber contract: a dependency changed, or this.refresh() was called. */
  run(): void {
    if (this.disposed || this.#finished || this.#broken || !this.#fiber) return

    // Already resuming — reached by a `flushSync()` from inside a render body.
    // Going on would clear the in-progress dep set and then throw "Generator is
    // executing", which `run`'s own catch reads as a render failure and kills
    // the component. Note the resume and come back once it has finished:
    // rescheduling here instead would be picked straight back up by the very
    // flush we are nested inside, and spin until its pass limit.
    if (this.#resumeDeps !== null) {
      this.#needsResume = true
      return
    }

    // A parked async component is driven from here like a sync one: the thunk
    // is the render, and there is nothing left to await.
    if (!this.isAsync || this.#thunk !== null) {
      const outer = currentInstance
      currentInstance = this
      enterReconcile(this.#host)
      try {
        let view: View | typeof NO_VIEW
        try {
          view = this.#resumeSync()
        } catch (error) {
          // A looping component built its view inside `gen.next()`, so its
          // generator is gone and it cannot be its own boundary. A parked one
          // is alive and suspended at its yield, so it gets first refusal.
          this.#fail(error)
          return
        }
        if (view !== NO_VIEW) this.#commit(view)
      } finally {
        currentInstance = outer
        leaveReconcile()
        // A write from inside the render scheduled us while we were mid-resume.
        // The enclosing flush is still looping, so this is picked up on its
        // next pass rather than being dropped.
        if (this.#needsResume) {
          this.#needsResume = false
          if (!this.disposed && !this.#finished) scheduleUpdate(this)
        }
      }
      return
    }

    if (this.#looping) {
      // Coalesce: only the latest props survive the wait.
      this.#pending = this.#props
      return
    }
    void this.#loop().catch(reportError)
  }

  /**
   * Patch this component's subtree against a new view.
   *
   * Sets `currentInstance` because children are mounted from here, not from the
   * generator resume — the async loop reaches this without going through
   * `run()`, so the window cannot live only at the call sites.
   */
  #commit(view: View | typeof NO_VIEW): void {
    if (view === NO_VIEW || !this.#fiber) return
    const fiber = this.#fiber
    const outer = currentInstance
    currentInstance = this
    enterReconcile(this.#host)
    try {
      fiber.child = patch(this.#host, fiber.child, view, this.#parent, this.depth + 1)
    } catch (error) {
      // Something in the subtree threw without being caught at its own
      // instance — a ref, a prop assignment, a plain element. This generator is
      // alive and suspended at its yield, so it gets first refusal.
      queueFailure(this, error)
    } finally {
      currentInstance = outer
      leaveReconcile()
    }
  }

  dispose(): void {
    if (this.disposed) return
    this.disposed = true

    // Unwinds `using` scopes and `finally` blocks, and — because yield*
    // forwards return() into delegates — every behavior it delegated to.
    // On an async generator this returns a promise, so a rejection during
    // `await using` teardown has to be routed rather than left to escape.
    //
    // `yield` inside a `finally` is legal, and `return()` on a generator parked
    // there comes back `{ done: false }` — still alive, still suspended, with
    // every *enclosing* `using` scope un-run. One call is not enough: keep
    // returning until it completes, or the resources those scopes hold are
    // stranded for good.
    const gen = this.#gen
    if (gen !== null) {
      try {
        let returned: unknown = gen.return(undefined as never)
        if (isThenable(returned)) {
          returned.then(undefined, reportError)
        } else {
          let guard = 0
          while (!(returned as IteratorResult<unknown>).done) {
            if (++guard > TEARDOWN_LIMIT) {
              if (DEV) {
                console.warn(
                  `view: <${this.#def.name}> is still yielding after ${String(TEARDOWN_LIMIT)} ` +
                    'teardown attempts. A `yield` in a `finally` runs during unmount, ' +
                    'and a loop around one never lets the generator finish — so its ' +
                    '`using` scopes never unwind.',
                )
              }
              break
            }
            returned = gen.return(undefined as never)
          }
        }
      } catch (error) {
        reportError(error)
      }
    }

    // Only components that actually read `this.aborted` ever have a controller.
    this.#abort?.abort()
    unsubscribeAll(this, this.#deps)
    this.#gen = null
    this.#fiber = null
  }
}

/** Recovers the instance a `propsView` trap is firing for. See `PROPS_VIEW_HANDLER`. */
const instanceForTarget = new WeakMap<object, Instance>()

/**
 * One `ProxyHandler`, shared by every component's `propsView` — the same
 * "closures cost per instance, prototype methods don't" trade `InstanceCtx`
 * already makes below, applied to the five Proxy traps instead of `Ctx`'s
 * methods. Each instance still allocates its own opaque target (see
 * `propsView`'s comment for why), just not its own copy of these five
 * functions.
 */
const PROPS_VIEW_HANDLER: ProxyHandler<object> = {
  get(target, key): unknown {
    const inst = instanceForTarget.get(target) as Instance
    if (typeof key === 'string') inst.notePropRead(key)
    return (inst.liveProps as Record<string | symbol, unknown>)[key]
  },
  has(target, key): boolean {
    const inst = instanceForTarget.get(target) as Instance
    if (typeof key === 'string') inst.notePropRead(key)
    return key in inst.liveProps
  },
  ownKeys(target): ArrayLike<string | symbol> {
    const inst = instanceForTarget.get(target) as Instance
    // A spread or `Object.keys` reads everything, so it depends on
    // everything. Recording each key is what makes that true rather than
    // merely plausible.
    const keys = Reflect.ownKeys(inst.liveProps)
    for (const key of keys) if (typeof key === 'string') inst.notePropRead(key)
    return keys
  },
  getOwnPropertyDescriptor(target, key): PropertyDescriptor | undefined {
    const inst = instanceForTarget.get(target) as Instance
    const descriptor = Reflect.getOwnPropertyDescriptor(inst.liveProps, key)
    // The target has no own properties, so anything reported back must be
    // configurable or the invariant check throws.
    return descriptor === undefined ? undefined : { ...descriptor, configurable: true }
  },
  set(target): boolean {
    if (DEV) {
      const inst = instanceForTarget.get(target) as Instance
      throw new Error(
        `view: <${inst.displayName}> assigned to a prop. Props are owned by the ` +
          'parent and are read-only here; hold state the parent does not ' +
          'describe in a signal or a generator-scope variable.',
      )
    }
    // Stripped in production: the write is dropped rather than corrupting
    // the parent's descriptor, which is the same outcome the throw has.
    return true
  },
}

/**
 * The per-component context object.
 *
 * A class rather than an object literal so the methods live on the prototype:
 * one instance is created per mounted component, and at ten thousand rows the
 * difference between one field and five closures is the difference between ten
 * thousand allocations and fifty thousand.
 *
 * Deliberately holds no cleanup API. Teardown is `using` and `try`/`finally` in
 * the generator body — the body is a block that stays open across `yield`, so
 * `gen.return()` on unmount unwinds it, and there is nothing left for a
 * framework method to add.
 */
class InstanceCtx implements Ctx {
  readonly #inst: Instance

  constructor(inst: Instance) {
    this.#inst = inst
  }

  refresh(): void {
    scheduleUpdate(this.#inst)
  }

  get aborted(): AbortSignal {
    return this.#inst.abortSignal
  }

  track<T>(fn: () => T): T {
    return this.#inst.trackIn(fn)
  }

  provide<T>(key: Context<T>, value: T): void {
    this.#inst.provideContext(key, value)
  }

  inject<T>(key: Context<T>): T {
    return this.#inst.injectContext(key)
  }
}

function reportError(error: unknown): void {
  queueMicrotask(() => {
    throw error
  })
}

function isThenable(value: unknown): value is PromiseLike<unknown> {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as PromiseLike<unknown>).then === 'function'
  )
}

// ---------------------------------------------------------------------------
// Mount
// ---------------------------------------------------------------------------

/**
 * Build a fiber and insert its DOM node.
 *
 * `parent` is the element the fiber logically belongs to and is what later
 * patches will insert into; `into` is where this node is physically placed
 * right now. They differ only when a run of siblings is being staged in a
 * `DocumentFragment`, which is why the two cannot be collapsed — a component
 * that later changes its root node type must re-mount into the real parent,
 * not into a fragment that was discarded after insertion.
 */
function mount(
  host: Host,
  vnode: Child,
  parent: HostEl,
  before: HostNode | null,
  depth: number,
  into: HostEl = parent,
): Fiber {
  if (vnode === null || vnode === undefined || typeof vnode === 'boolean') {
    const dom = host.createHole()
    host.insert(into, dom, before)
    return { kind: 'hole', key: undefined, dom }
  }

  if (typeof vnode === 'string' || typeof vnode === 'number') {
    const value = String(vnode)
    const dom = host.createText(value)
    host.insert(into, dom, before)
    return { kind: 'text', key: undefined, dom, value }
  }

  if (DEV && typeof (vnode as unknown) === 'function') {
    // Only a component may yield a render thunk, and only as its whole view.
    // Without this the function falls through to the element branch with no
    // `tag`, and builds an `<undefined>` element that `sameType` then remounts
    // on every patch — a silent failure rather than a message.
    throw new Error(
      'view: a function reached a child position. A render thunk is yielded, ' +
        'not nested — `yield () => div(…)`. A view built by a helper has to be ' +
        'called: `div(row())`, not `div(row)`.',
    )
  }

  if (Array.isArray(vnode)) {
    // A whole view that is an array: a component's root, or a render() root.
    // Arrays in a child slot never get here — `flatten` splices them first.
    const kids = fragmentChildren(vnode as readonly Child[])
    const children: Fiber[] = new Array(kids.length) as Fiber[]
    let built = 0
    try {
      for (; built < kids.length; built++) {
        children[built] = mount(host, kids[built] as Child, parent, before, depth, into)
      }
    } catch (error) {
      // These siblings went straight into `into`, so they need detaching too.
      for (let i = 0; i < built; i++) unmount(host, children[i] as Fiber)
      throw error
    }
    return { kind: 'frag', key: undefined, children, vnode: vnode as readonly Child[] }
  }

  const node = vnode as ElementNode

  if (isComponentDef(node.tag)) {
    const inst = new Instance(node.tag, node.props, host, parent, depth)
    const fiber: CompFiber = {
      kind: 'comp',
      key: node.key,
      def: node.tag,
      inst,
      child: null as unknown as Fiber,
      vnode: node,
    }
    inst.attach(fiber)
    try {
      fiber.child = inst.mount(before, into)
    } catch (error) {
      // The generator already ran to its first yield, so it may hold `using`
      // scopes and signal subscriptions. Nothing will ever reach this instance
      // again — unwind it here or it is pinned for the life of the sources it
      // read.
      inst.dispose()
      throw error
    }
    return fiber
  }

  if (isPortal(node.tag)) {
    if (!host.supportsPortals) {
      throw new Error(
        'view: this renderer has no portals. They exist to escape an `overflow` ' +
          'or `z-index` context, and a host without either has nothing to escape ' +
          '— reorder the children instead.',
      )
    }
    // A placeholder keeps the portal's place among its logical siblings, so
    // they have something to anchor against; the content goes elsewhere.
    const placeholder = host.createHole()
    host.insert(into, placeholder, before)

    const target = node.props['target'] as HostEl
    const tail = host.createHole()
    host.append(target, tail)

    const kids = flatten(node.children)
    const children: Fiber[] = new Array(kids.length) as Fiber[]
    try {
      mountRange(host, target, kids, 0, kids.length, tail, depth + 1, children)
    } catch (error) {
      // Portal content lives in a foreign target, so nothing else will ever
      // reach it — `disposeTree`'s portal branch is the only pass that does,
      // and this fiber is never returned for it to find.
      for (const child of children) if (child !== undefined) unmount(host, child)
      host.remove(tail)
      host.remove(placeholder)
      throw error
    }

    return {
      kind: 'portal',
      key: node.key,
      dom: placeholder,
      tail,
      target,
      children,
      vnode: node,
    }
  }

  const tag = node.tag as string
  const dom = host.createNode(tag)
  // Pre-sized and filled by index, like the array-vnode and portal branches
  // above: a row's `<tr>` mounts with a known child count, and growing the
  // array one `push` at a time reallocates as it goes for no reason.
  let children: Fiber[] = []
  let built = 0
  let detach: (() => void) | undefined
  let inserted = false
  try {
    host.applyProps(dom, node.props)
    const kids = flatten(node.children)
    children = new Array(kids.length) as Fiber[]
    for (; built < kids.length; built++) {
      children[built] = mount(host, kids[built] as Child, dom, null, depth + 1)
    }
    host.insert(into, dom, before)
    inserted = true
    detach = host.runRef(node.props, dom) ?? undefined
  } catch (error) {
    // `applyProps`, a `ref`, or any child's own mount can throw. The fibers
    // built so far are referenced by nothing — this one never gets returned —
    // so `disposeTree` could never reach them again, and every `using` scope
    // and subscription beneath would leak with no way to unwind it.
    // Removing `dom` takes their nodes with it, so only disposal is needed.
    // Only `built` slots are ever filled — the rest of a pre-sized array is
    // holes, and iterating past `built` would hand `disposeTree` `undefined`.
    for (let i = 0; i < built; i++) disposeTree(host, children[i] as Fiber)
    if (inserted) host.remove(dom)
    throw error
  }
  return { kind: 'el', key: node.key, dom, tag, children, detach, vnode: node }
}

/**
 * Mount `vnodes[from, to)` before `anchor`.
 *
 * More than one node is staged in the host's fragment and inserted in a single
 * call. Each node's own subtree is already built detached, so this is only
 * about the top-level siblings — but at a thousand rows that is the difference
 * between one live insertion and a thousand.
 *
 * A host with no fragment says so by returning null, and each sibling is
 * inserted directly. Inserting them all before the *same* anchor keeps them in
 * order, since each lands immediately after the one before it.
 */
function mountRange(
  host: Host,
  parent: HostEl,
  vnodes: readonly Child[],
  from: number,
  to: number,
  anchor: HostNode | null,
  depth: number,
  out: Fiber[],
): void {
  const count = to - from
  if (count <= 0) return
  if (count === 1) {
    out[from] = mount(host, vnodes[from] as Child, parent, anchor, depth)
    return
  }
  const frag = host.createFragment()
  if (frag === null) {
    let i = from
    try {
      for (; i < to; i++) out[i] = mount(host, vnodes[i] as Child, parent, anchor, depth)
    } catch (error) {
      // Inserted one by one, so each needs detaching as well as disposing.
      for (let j = from; j < i; j++) unmount(host, out[j] as Fiber)
      throw error
    }
    return
  }
  let staged = from
  try {
    for (; staged < to; staged++) {
      out[staged] = mount(host, vnodes[staged] as Child, parent, null, depth, frag)
    }
  } catch (error) {
    // The fragment was never inserted, so these nodes go out of scope with it
    // and only their fibers need unwinding.
    for (let j = from; j < staged; j++) disposeTree(host, out[j] as Fiber)
    throw error
  }
  host.insert(parent, frag, anchor)
}

// ---------------------------------------------------------------------------
// Unmount
// ---------------------------------------------------------------------------

/**
 * Dispose component instances in a subtree.
 *
 * It leaves the subtree's own DOM alone, because the caller removes that in one
 * go — often as a single `textContent` write. Portal content is the exception:
 * it lives in a foreign element that no caller of this will ever clear, and
 * this is the only pass that reaches the whole tree, so it is detached here.
 */
function disposeTree(host: Host, fiber: Fiber): void {
  if (fiber.kind === 'comp') {
    // Child first, then this component — the order Solid, React and Vue all
    // unwind in, and for the same reason: a child's teardown may touch
    // something its parent owns (a context value, a connection, a subscription
    // it was handed), so disposing the parent first would hand the child a
    // resource that has already been closed. The reverse dependency does not
    // exist: a parent never reaches into a child's state.
    disposeTree(host, fiber.child)
    fiber.inst.dispose()
    return
  }
  if (fiber.kind === 'portal') {
    for (const child of fiber.children) disposeTree(host, child)
    for (const child of fiber.children) removeDom(host, child)
    host.remove(fiber.tail)
    return
  }
  if (fiber.kind === 'el' || fiber.kind === 'frag') {
    for (const child of fiber.children) disposeTree(host, child)
    if (fiber.kind === 'el' && fiber.detach !== undefined) {
      const detach = fiber.detach
      // Cleared first: disposal must stay idempotent, and a `ref` that throws
      // must not strand the rest of the teardown.
      fiber.detach = undefined
      try {
        detach()
      } catch (error) {
        reportError(error)
      }
    }
  }
}

function unmount(host: Host, fiber: Fiber): void {
  disposeTree(host, fiber)
  removeDom(host, fiber)
}

/**
 * Remove a set of sibling fibers.
 *
 * When they are every child `parent` has, one `clear` replaces N `remove` calls
 * — the difference between 10k host operations and one when a large list is
 * cleared or wholly replaced. The child-count check is what keeps that safe in
 * the presence of nodes a `ref` may have appended.
 *
 * It also covers fragments for free. Every fiber owns at least one node, so the
 * counts can only match when each owns exactly one; a multi-node fragment in
 * the list pushes the node count higher and the bulk path is simply skipped.
 *
 * `end` non-null means the fibers own a slice of `parent` rather than all of
 * it, which rules the bulk path out regardless.
 */
function unmountAll(
  host: Host,
  parent: HostEl,
  fibers: readonly Fiber[],
  end: HostNode | null,
): void {
  const count = fibers.length
  if (count === 0) return

  for (let i = 0; i < count; i++) disposeTree(host, fibers[i] as Fiber)

  if (end === null && host.childCount(parent) === count) {
    host.clear(parent)
    return
  }
  for (let i = 0; i < count; i++) removeDom(host, fibers[i] as Fiber)
}

// ---------------------------------------------------------------------------
// Patch
// ---------------------------------------------------------------------------

function patch(
  host: Host,
  fiber: Fiber,
  vnode: Child,
  parent: HostEl,
  depth: number,
): Fiber {
  // Identity bail. A generator that yields the same descriptor object twice is
  // saying nothing changed, so the whole subtree can be skipped — which makes
  // hoisted static views free. Safe because a descendant component with pending
  // signal updates reaches the scheduler directly, not through its parent.
  if (
    (fiber.kind === 'el' ||
      fiber.kind === 'comp' ||
      fiber.kind === 'frag' ||
      fiber.kind === 'portal') &&
    fiber.vnode === vnode
  ) {
    return fiber
  }

  if (!sameType(fiber, vnode)) {
    const before = domOf(fiber)
    const next = mount(host, vnode, parent, before, depth)
    unmount(host, fiber)
    return next
  }

  if (fiber.kind === 'hole') return fiber

  if (fiber.kind === 'text') {
    const value = String(vnode)
    if (fiber.value !== value) {
      host.setText(fiber.dom, value)
      fiber.value = value
    }
    return fiber
  }

  if (fiber.kind === 'frag') {
    // Where this fragment ends, read before anything moves. The node after it
    // is outside the fragment, so it survives whatever happens inside; null
    // correctly means "nothing follows, append to the parent".
    const end = host.nextSibling(lastDomOf(fiber))
    const next = vnode as readonly Child[]
    fiber.vnode = next
    fiber.children = patchChildren(
      host,
      parent,
      fiber.children,
      fragmentChildren(next),
      depth,
      end,
    )
    return fiber
  }

  const node = vnode as ElementNode

  if (fiber.kind === 'comp') {
    fiber.vnode = node
    fiber.inst.update(node.props)
    return fiber
  }

  if (fiber.kind === 'portal') {
    const target = node.props['target'] as HostEl
    if (target !== fiber.target) {
      // Retargeting: tear the old content down and rebuild in the new element.
      // Reusing the fibers would mean re-parenting each node by hand for no
      // gain, since a portal changing target is rare.
      for (const child of fiber.children) disposeTree(host, child)
      for (const child of fiber.children) removeDom(host, child)
      host.remove(fiber.tail)

      const tail = host.createHole()
      host.append(target, tail)
      const kids = flatten(node.children)
      const children: Fiber[] = new Array(kids.length) as Fiber[]
      mountRange(host, target, kids, 0, kids.length, tail, depth + 1, children)

      fiber.target = target
      fiber.tail = tail
      fiber.children = children
      fiber.vnode = node
      return fiber
    }

    fiber.vnode = node
    // The portal owns the run up to its own tail, not all of `target`.
    try {
      fiber.children = patchChildren(
        host,
        target,
        fiber.children,
        flatten(node.children),
        depth + 1,
        fiber.tail,
      )
    } catch (error) {
      // `patchChildren` unwound the whole run, so the old list describes
      // nothing that is still in the tree.
      fiber.children = NO_FIBERS
      throw error
    }
    return fiber
  }

  // Read through the outgoing `vnode` before it is overwritten below — its
  // `.props` is the fiber's only copy of what was last applied.
  host.applyProps(fiber.dom, node.props, fiber.vnode.props)
  fiber.vnode = node
  // An element owns every one of its children, so the list runs to its end.
  try {
    fiber.children = patchChildren(
      host,
      fiber.dom,
      fiber.children,
      flatten(node.children),
      depth + 1,
      null,
    )
  } catch (error) {
    // `patchChildren` unwound the whole run, so the old list describes nothing
    // that is still in the tree. Keeping it is what left a boundary's fallback
    // patching into detached nodes.
    fiber.children = NO_FIBERS
    throw error
  }
  return fiber
}

/** A fiber can be reused for a vnode only if both key and type line up. */
function reusable(fiber: Fiber, vnode: Child): boolean {
  return fiber.key === keyOf(vnode) && sameType(fiber, vnode)
}

const NO_FIBERS: Fiber[] = []

/**
 * Keyed children reconciliation.
 *
 * Matching ends where the two lists stop agreeing: a common prefix and suffix
 * are patched in place first, and only the disputed middle goes through the
 * key map. Within that middle, the nodes that form the longest increasing
 * subsequence of old positions are already in relative order and stay put, so
 * a two-row swap moves two nodes rather than cascading through the list.
 *
 * `end` is the node these children run up to: null when they are everything
 * `parent` has, and the fragment's following sibling when they are only a
 * slice of it.
 */
function patchChildren(
  host: Host,
  parent: HostEl,
  oldFibers: Fiber[],
  vnodes: readonly Child[],
  depth: number,
  end: HostNode | null,
): Fiber[] {
  const newLen = vnodes.length

  if (newLen === 0) {
    unmountAll(host, parent, oldFibers, end)
    return NO_FIBERS
  }

  // Fast path: same length, and every old fiber reuses at its own index — no
  // insertion, removal, or reorder anywhere in the list, which is the common
  // case for a list whose *rows* changed but whose row count and order did
  // not (`update`, `select`). Checked read-only first — `reusable` has no
  // side effects — so a mismatch found part-way through never leaves
  // anything patched; only once every index is confirmed reusable does the
  // second loop run `patch`, in place on `oldFibers`, and skip the `result`
  // allocation-and-copy below entirely.
  if (oldFibers.length === newLen) {
    let allReusable = true
    for (let i = 0; i < newLen; i++) {
      if (!reusable(oldFibers[i] as Fiber, vnodes[i] as Child)) {
        allReusable = false
        break
      }
    }
    if (allReusable) {
      try {
        for (let i = 0; i < newLen; i++) {
          oldFibers[i] = patch(host, oldFibers[i] as Fiber, vnodes[i] as Child, parent, depth)
        }
      } catch (error) {
        // Same collapse as the general path's catch below: every slot still
        // holds a fiber describing what is currently mounted at that
        // position — the ones `patch` has not reached yet are still their
        // original fiber, the ones it has are patched in place — so
        // unmounting the whole array, patched or not, is exact.
        for (const fiber of oldFibers) unmount(host, fiber)
        throw error
      }
      return oldFibers
    }
  }

  const result: Fiber[] = new Array(newLen) as Fiber[]
  try {
    return reconcileChildren(host, parent, oldFibers, vnodes, depth, end, result)
  } catch (error) {
    // Part-way through: some old nodes are already detached, some new ones are
    // inserted, and `result` describes only a slice of either. Leaving the
    // parent fiber pointing at that mix is what makes a caught error permanent
    // — every later render writes into nodes that are no longer in the tree.
    // Collapse the whole run instead and let the boundary rebuild from empty.
    // `host.remove` ignores an already-detached node, so this is safe to run
    // over fibers the algorithm had already dropped.
    for (const old of oldFibers) unmount(host, old)
    for (const built of result) if (built !== undefined) unmount(host, built)
    throw error
  }
}

/** `patchChildren`'s body. Split out so the wrapper above owns the unwinding. */
function reconcileChildren(
  host: Host,
  parent: HostEl,
  oldFibers: Fiber[],
  vnodes: readonly Child[],
  depth: number,
  end: HostNode | null,
  result: Fiber[],
): Fiber[] {
  const oldLen = oldFibers.length
  const newLen = vnodes.length

  if (oldLen === 0) {
    mountRange(host, parent, vnodes, 0, newLen, end, depth, result)
    return result
  }

  let start = 0
  let oldEnd = oldLen - 1
  let newEnd = newLen - 1

  // 1. Common prefix.
  while (start <= oldEnd && start <= newEnd) {
    const old = oldFibers[start] as Fiber
    const vnode = vnodes[start] as Child
    if (!reusable(old, vnode)) break
    result[start] = patch(host, old, vnode, parent, depth)
    start++
  }

  // 2. Common suffix.
  while (start <= oldEnd && start <= newEnd) {
    const old = oldFibers[oldEnd] as Fiber
    const vnode = vnodes[newEnd] as Child
    if (!reusable(old, vnode)) break
    result[newEnd] = patch(host, old, vnode, parent, depth)
    oldEnd--
    newEnd--
  }

  const tailAnchor = (index: number): HostNode | null =>
    index + 1 < newLen ? domOf(result[index + 1] as Fiber) : end

  if (start > oldEnd) {
    // 3. Pure insertion: everything old was consumed by the two scans.
    mountRange(host, parent, vnodes, start, newEnd + 1, tailAnchor(newEnd), depth, result)
    return result
  }

  if (start > newEnd) {
    // 4. Pure removal. Reaching here means the two scans matched every new
    // vnode, so this slice is strictly smaller than the child list and the
    // bulk-clear guard inside `unmountAll` cannot fire.
    unmountAll(host, parent, oldFibers.slice(start, oldEnd + 1), end)
    return result
  }

  // 5. Disputed middle. Match old fibers to new positions by key, recording
  // where each survivor came from so the move set can be minimised below.
  const keyToNewIndex = new Map<Key, number>()
  for (let i = start; i <= newEnd; i++) {
    const key = keyOf(vnodes[i] as Child)
    if (key !== undefined) keyToNewIndex.set(key, i)
  }

  const middleLen = newEnd - start + 1
  // 0 means "nothing reusable here, mount fresh"; otherwise old index + 1.
  const newToOld = new Int32Array(middleLen)
  const removals: Fiber[] = []
  let matched = 0
  let moved = false
  let highWater = 0
  let unkeyedCursor = start

  for (let i = start; i <= oldEnd; i++) {
    const old = oldFibers[i] as Fiber
    if (matched >= middleLen) {
      removals.push(old)
      continue
    }

    let newIndex: number | undefined
    if (old.key !== undefined) {
      newIndex = keyToNewIndex.get(old.key)
      // Duplicate keys: an earlier old fiber already claimed this slot. Letting
      // the second one claim it too overwrites `result[newIndex]`, and the
      // first fiber then appears in neither `result` nor `removals` — never
      // disposed, its nodes never removed, and still subscribed and rendering.
      if (newIndex !== undefined && newToOld[newIndex - start] !== 0) newIndex = undefined
    } else {
      // Unkeyed fibers match positionally against the unkeyed slots still
      // available. The cursor only ever advances, keeping this linear.
      while (unkeyedCursor <= newEnd && keyOf(vnodes[unkeyedCursor] as Child) !== undefined) {
        unkeyedCursor++
      }
      if (unkeyedCursor <= newEnd) {
        newIndex = unkeyedCursor
        unkeyedCursor++
      }
    }

    if (newIndex === undefined || !sameType(old, vnodes[newIndex] as Child)) {
      removals.push(old)
      continue
    }

    newToOld[newIndex - start] = i + 1
    if (newIndex >= highWater) highWater = newIndex
    else moved = true
    result[newIndex] = patch(host, old, vnodes[newIndex] as Child, parent, depth)
    matched++
  }

  // Drop the unmatched before inserting, so a wholesale replacement collapses
  // into a single bulk clear rather than 1000 interleaved insert/remove pairs.
  if (removals.length === oldLen) unmountAll(host, parent, removals, end)
  else for (let i = 0; i < removals.length; i++) unmount(host, removals[i] as Fiber)

  if (matched === 0) {
    mountRange(host, parent, vnodes, start, newEnd + 1, tailAnchor(newEnd), depth, result)
    return result
  }

  const keep = moved ? longestIncreasingSubsequence(newToOld) : NO_SEQUENCE
  let k = keep.length - 1
  for (let i = middleLen - 1; i >= 0; i--) {
    const newIndex = start + i
    if (newToOld[i] === 0) {
      result[newIndex] = mount(
        host,
        vnodes[newIndex] as Child,
        parent,
        tailAnchor(newIndex),
        depth,
      )
    } else if (moved) {
      if (k < 0 || i !== keep[k]) {
        moveFiber(host, result[newIndex] as Fiber, parent, tailAnchor(newIndex))
      } else {
        k--
      }
    }
  }

  return result
}

const NO_SEQUENCE: number[] = []

/**
 * Indices of `arr` forming a longest increasing subsequence, ascending.
 * Zero entries are skipped: they mark positions with no old node to move.
 *
 * The nodes at these indices are already in the right relative order, so
 * everything else is the minimal set that has to be re-inserted.
 */
function longestIncreasingSubsequence(arr: Int32Array): number[] {
  const n = arr.length
  const parent = new Int32Array(n).fill(-1)
  // tails[l] holds the index into `arr` of the smallest tail among the
  // increasing subsequences of length l + 1 found so far.
  const tails: number[] = []

  for (let i = 0; i < n; i++) {
    const value = arr[i] as number
    if (value === 0) continue
    let lo = 0
    let hi = tails.length
    while (lo < hi) {
      const mid = (lo + hi) >> 1
      if ((arr[tails[mid] as number] as number) < value) lo = mid + 1
      else hi = mid
    }
    if (lo > 0) parent[i] = tails[lo - 1] as number
    tails[lo] = i
  }

  let length = tails.length
  const out: number[] = new Array(length) as number[]
  let cursor = length > 0 ? (tails[length - 1] as number) : -1
  while (length-- > 0) {
    out[length] = cursor
    cursor = parent[cursor] as number
  }
  return out
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

export interface Root {
  /** Replace the rendered view. */
  update(view: View): void
  /** Unmount everything and release all resources. */
  unmount(): void
}

/**
 * Render into any host.
 *
 * Every backend is this with a host bound: `render()` in `../dom/render.ts`,
 * `renderCanvas` in `../canvas/index.ts`, `renderTui` in `../tui/index.ts`.
 * Each is a handful of lines around this one.
 *
 * Nothing host-specific may enter this module. Binding the DOM host here — as
 * `render()` used to — pins `domHost` into every bundle that reaches the
 * reconciler, including the ones that paint to a canvas or a terminal and have
 * no DOM at all. `scripts/boundaries.test.ts` holds that line.
 */
export function renderWith(view: View, container: HostEl, host: Host): Root {
  host.clear(container)
  // The outermost reconcile frame. A failure with no boundary above it drains
  // here and rethrows, so the caller of `render()` sees it.
  enterReconcile(host)
  let fiber: Fiber
  try {
    fiber = mount(host, view, container, null, 0)
  } finally {
    leaveReconcile()
  }
  return {
    update(next: View) {
      enterReconcile(host)
      try {
        fiber = patch(host, fiber, next, container, 0)
      } finally {
        leaveReconcile()
      }
    },
    unmount() {
      enterReconcile(host)
      try {
        unmount(host, fiber)
      } finally {
        leaveReconcile()
      }
    },
  }
}
