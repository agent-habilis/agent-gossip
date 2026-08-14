/**
 * Reactive core.
 *
 * The central idea of this library: **the yield boundary is the reactive
 * boundary**. Other signal libraries need a wrapper function to delimit
 * dependency tracking. A generator's suspension points delimit it for free —
 * everything read between "resume" and the next `yield` is a dependency of that
 * yield. No wrapper, no dependency array, no memoization.
 *
 * `track()` here opens that window; the component driver closes it at the yield.
 */

/** Anything that can depend on a signal and be re-run. */
export interface Subscriber {
  /** Tree depth; parents flush before children so a child is not run twice. */
  readonly depth: number
  /** Re-run this subscriber. */
  run(): void
  /** Set once the subscriber is torn down; scheduling becomes a no-op. */
  disposed: boolean
}

interface Source {
  readonly subs: Set<Subscriber>
}

let currentDeps: Set<Source> | null = null

/** Open a tracking window. Returns the previous one so it can be restored. */
export function openTracking(deps: Set<Source>): Set<Source> | null {
  const prev = currentDeps
  currentDeps = deps
  return prev
}

export function closeTracking(prev: Set<Source> | null): void {
  currentDeps = prev
}

/** Run `fn` outside any tracking window — reads inside will not subscribe. */
export function untrack<T>(fn: () => T): T {
  const prev = currentDeps
  currentDeps = null
  try {
    return fn()
  } finally {
    currentDeps = prev
  }
}

/** Run `fn` inside an explicit tracking window, collecting into `deps`. */
export function trackInto<T>(deps: Set<Source>, fn: () => T): T {
  const prev = openTracking(deps)
  try {
    return fn()
  } finally {
    closeTracking(prev)
  }
}

/**
 * Record a read of `source` against the open tracking window, if any.
 *
 * Props are tracked too, but not through here: `reconcile.ts` hands components
 * a Proxy that notes which keys were read and asks `isTracking()` whether the
 * window is open. Same rule, cheaper mechanism — a prop is compared against the
 * parent's next descriptor rather than carrying a subscriber set of its own.
 */
export function record(source: Source): void {
  currentDeps?.add(source)
}

/**
 * Is a tracking window open right now?
 *
 * The props layer asks this before recording a prop read, so that a prop obeys
 * exactly the rule a signal does: it subscribes when read inside a window, and
 * does not when read under `untrack` or outside a resume entirely.
 */
export function isTracking(): boolean {
  return currentDeps !== null
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

const queue = new Set<Subscriber>()
let flushScheduled = false
let batchDepth = 0

export function scheduleUpdate(sub: Subscriber): void {
  if (sub.disposed) return
  queue.add(sub)
  if (batchDepth > 0 || flushScheduled) return
  flushScheduled = true
  queueMicrotask(flush)
}

function flush(): void {
  flushScheduled = false
  // Guard against a subscriber scheduling more work as it runs.
  let guard = 0
  while (queue.size > 0) {
    if (guard++ > 100) {
      queue.clear()
      throw new Error('view: update loop did not settle after 100 passes')
    }
    // Shallowest first: a parent re-render may unmount queued children.
    const batch = [...queue].sort((a, b) => a.depth - b.depth)
    queue.clear()
    for (const sub of batch) {
      if (sub.disposed) continue
      try {
        sub.run()
      } catch (error) {
        // The queue was drained before the batch ran, so letting this escape
        // would silently drop every subscriber after it. Report and carry on.
        queueMicrotask(() => {
          throw error
        })
      }
    }
  }
}

/** Apply many writes, then flush once. */
export function batch<T>(fn: () => T): T {
  batchDepth += 1
  try {
    return fn()
  } finally {
    batchDepth -= 1
    if (batchDepth === 0 && queue.size > 0 && !flushScheduled) {
      flushScheduled = true
      queueMicrotask(flush)
    }
  }
}

/** Run all pending updates immediately. Useful in tests. */
export function flushSync(): void {
  flush()
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

export interface ReadonlySignal<T> {
  readonly value: T
  peek(): T
}

export interface Signal<T> extends ReadonlySignal<T> {
  value: T
  /** Update from the previous value. */
  update(fn: (prev: T) => T): void
}

class SignalImpl<T> implements Signal<T> {
  readonly subs = new Set<Subscriber>()
  #value: T

  constructor(initial: T) {
    this.#value = initial
  }

  get value(): T {
    record(this)
    return this.#value
  }

  set value(next: T) {
    if (Object.is(next, this.#value)) return
    this.#value = next
    this.#notify()
  }

  peek(): T {
    return this.#value
  }

  update(fn: (prev: T) => T): void {
    this.value = fn(this.#value)
  }

  #notify(): void {
    if (this.subs.size === 0) return
    for (const sub of [...this.subs]) scheduleUpdate(sub)
  }
}

export function signal<T>(initial: T): Signal<T> {
  return new SignalImpl(initial)
}

// ---------------------------------------------------------------------------
// Computed
// ---------------------------------------------------------------------------

class ComputedImpl<T> implements ReadonlySignal<T>, Subscriber {
  readonly subs = new Set<Subscriber>()
  readonly depth = -1 // computeds resolve lazily on read, not via the queue
  disposed = false

  #deps = new Set<Source>()
  /** Double-buffered like `Instance#depsAlt`: swapped and cleared per recomputation, not reallocated. */
  #depsAlt = new Set<Source>()
  #value!: T
  #stale = true

  constructor(private readonly compute: () => T) {}

  get value(): T {
    record(this)
    if (this.#stale) this.#recompute()
    return this.#value
  }

  peek(): T {
    if (this.#stale) this.#recompute()
    return this.#value
  }

  /** A dependency changed: mark stale and propagate to our own subscribers. */
  run(): void {
    if (this.#stale) return
    this.#stale = true
    for (const sub of [...this.subs]) scheduleUpdate(sub)
  }

  #recompute(): void {
    for (const dep of this.#deps) dep.subs.delete(this)
    const deps = this.#depsAlt
    deps.clear()
    const next = trackInto(deps, this.compute)
    this.#depsAlt = this.#deps
    this.#deps = deps
    for (const dep of deps) dep.subs.add(this)
    this.#stale = false
    this.#value = next
  }
}

export function computed<T>(compute: () => T): ReadonlySignal<T> {
  return new ComputedImpl(compute)
}

/** Subscribe `sub` to exactly `next`, unsubscribing from anything it dropped. */
export function resubscribe(
  sub: Subscriber,
  prev: Set<Source>,
  next: Set<Source>,
): void {
  for (const dep of prev) {
    if (!next.has(dep)) dep.subs.delete(sub)
  }
  for (const dep of next) dep.subs.add(sub)
}

export function unsubscribeAll(sub: Subscriber, deps: Set<Source>): void {
  for (const dep of deps) dep.subs.delete(sub)
  deps.clear()
}

export type { Source }
