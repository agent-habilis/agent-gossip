/**
 * Disposable resources for `using`.
 *
 * Unmounting a component calls its generator's `return()`, which unwinds every
 * `using` scope in the body. These helpers are the things worth putting in one:
 *
 *     const Clock = component(function* () {
 *       using tick = interval(1000, () => now.value = Date.now())
 *       yield () => div(format(now.value))
 *     })
 *
 * Every helper returns a plain `Disposable` — nothing registers itself with an
 * ambient component, so they work outside one too, and disposal is always
 * explicit. For a resource whose lifetime cannot be a block — acquired inside
 * an `if`, or from a callback — hold a `DisposableStack` with `using` and add
 * to it.
 *
 * Disposal is idempotent throughout: disposing twice is a no-op, so a resource
 * that lands in two scopes cannot double-clear a timer id that the platform may
 * since have reused.
 */


/** Wrap a cleanup function so it runs at most once. */
function once(fn: () => void): Disposable {
  let done = false
  return {
    [Symbol.dispose]() {
      if (done) return
      done = true
      fn()
    },
  }
}

// ---------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------

/** `setInterval`, cleared on dispose. */
export function interval(ms: number, fn: () => void): Disposable {
  const id = setInterval(fn, ms)
  return once(() => clearInterval(id))
}

/** `setTimeout`, cancelled on dispose if it has not fired yet. */
export function timeout(ms: number, fn: () => void): Disposable {
  const id = setTimeout(fn, ms)
  return once(() => clearTimeout(id))
}

/**
 * A `requestAnimationFrame` loop. `fn` receives the milliseconds elapsed since
 * the previous frame, which is zero on the first one.
 */
export function animation(fn: (delta: number) => void): Disposable {
  let handle = 0
  let previous: number | null = null

  const frame = (now: number): void => {
    const delta = previous === null ? 0 : now - previous
    previous = now
    // Scheduled before the call, so disposing from inside `fn` cancels the
    // frame that is already queued rather than leaving one pending.
    handle = requestAnimationFrame(frame)
    try {
      fn(delta)
    } catch (error) {
      // Otherwise a throwing `fn` re-throws on every frame, forever.
      cancelAnimationFrame(handle)
      throw error
    }
  }

  handle = requestAnimationFrame(frame)
  return once(() => cancelAnimationFrame(handle))
}

// ---------------------------------------------------------------------------
// Polling
// ---------------------------------------------------------------------------

export interface PollOptions {
  /** Run once immediately rather than waiting out the first interval. Default true. */
  immediate?: boolean
  /** Called when a run throws or rejects. Default: report as an uncaught error. */
  onError?: (error: unknown) => void
}

/**
 * Poll `fn` every `ms`, waiting for each run to settle before scheduling the
 * next. This is the difference from `interval`: a run slower than the interval
 * delays the next one rather than stacking up alongside it.
 *
 * `fn` receives an `AbortSignal` that fires on disposal, so an in-flight request
 * is cancelled rather than left to land after unmount.
 */
export function poll(
  ms: number,
  fn: (signal: AbortSignal) => unknown,
  options: PollOptions = {},
): Disposable {
  const { immediate = true, onError } = options
  const controller = new AbortController()
  let timer: ReturnType<typeof setTimeout> | undefined
  let stopped = false

  const fail = (error: unknown): void => {
    if (onError === undefined) {
      queueMicrotask(() => { throw error })
      return
    }
    try {
      onError(error)
    } catch (thrown) {
      // A throwing `onError` used to escape `run` before it could reschedule,
      // killing the poll for good while its `Disposable` still looked healthy.
      queueMicrotask(() => { throw thrown })
    }
  }

  const schedule = (): void => {
    if (stopped) return
    timer = setTimeout(run, ms)
  }

  const run = async (): Promise<void> => {
    if (stopped) return
    try {
      await fn(controller.signal)
    } catch (error) {
      // Everything that fails after disposal is dropped, not just a
      // `DOMException` named AbortError: `fn` is free to reject with anything
      // it likes when the signal fires, and a teardown the caller asked for is
      // not a failure they need told about. The cost is that an unrelated
      // failure landing in the same window is swallowed too — deliberate, since
      // the alternative reports noise on every unmount.
      if (!controller.signal.aborted) fail(error)
    } finally {
      // In the `finally` so a throw from `fail` cannot strand the loop.
      schedule()
    }
  }

  if (immediate) void run()
  else schedule()

  return once(() => {
    stopped = true
    clearTimeout(timer)
    controller.abort()
  })
}

// ---------------------------------------------------------------------------
// DOM subscriptions
// ---------------------------------------------------------------------------

/**
 * `addEventListener`, removed on dispose. Typed per target through the DOM event
 * maps, so the handler's event type is inferred.
 *
 * Inside a component, `addEventListener(..., { signal: this.aborted })` does the
 * same job and stays the recommended form; this exists for use outside one.
 */
export function listen<K extends keyof WindowEventMap>(
  target: Window,
  type: K,
  handler: (event: WindowEventMap[K]) => void,
  options?: AddEventListenerOptions,
): Disposable
export function listen<K extends keyof DocumentEventMap>(
  target: Document,
  type: K,
  handler: (event: DocumentEventMap[K]) => void,
  options?: AddEventListenerOptions,
): Disposable
export function listen<K extends keyof HTMLElementEventMap>(
  target: HTMLElement,
  type: K,
  handler: (event: HTMLElementEventMap[K]) => void,
  options?: AddEventListenerOptions,
): Disposable
export function listen(
  target: EventTarget,
  type: string,
  handler: (event: Event) => void,
  options?: AddEventListenerOptions,
): Disposable
export function listen(
  target: EventTarget,
  type: string,
  handler: (event: never) => void,
  options?: AddEventListenerOptions,
): Disposable {
  const listener = handler as EventListener
  target.addEventListener(type, listener, options)
  return once(() => target.removeEventListener(type, listener, options))
}

/** The shape every DOM observer shares. */
export interface Observer {
  disconnect(): void
}

/**
 * Attach an observer to one or more targets and disconnect it on dispose.
 * Works with every DOM observer, which all share a `disconnect()` method.
 *
 *     using ro = observe(new ResizeObserver(onResize), element)
 *     using mo = observe(new MutationObserver(onMutate), element, { childList: true })
 *
 * Returns a plain `Disposable` rather than the observer itself. Handing back
 * `observer & Disposable` meant assigning `Symbol.dispose` onto the caller's
 * object: anything else holding that observer silently gained a dispose method,
 * and a `using` at a second site would tear down the first site's resource.
 * Keep your own reference if you need `unobserve`.
 */
export function observe(
  observer: Observer,
  targets: Node | readonly Node[],
  init?: MutationObserverInit,
): Disposable {
  // `MutationObserver.observe` throws without an init describing what to watch,
  // so it is forwarded rather than dropped — the "any observer" claim above was
  // not true before. The other observers ignore a second argument.
  const connect = observer as unknown as {
    observe?: (node: Node, init?: MutationObserverInit) => void
  }
  const list = Array.isArray(targets) ? (targets as readonly Node[]) : [targets as Node]
  for (const target of list) connect.observe?.(target, init)

  return once(() => observer.disconnect())
}

// ---------------------------------------------------------------------------
// Generic primitives
// ---------------------------------------------------------------------------

/** Turn any cleanup function into a `Disposable`. The escape hatch. */
export function disposable(fn: () => void): Disposable {
  return once(fn)
}

/** The `await using` counterpart of `disposable`. */
export function asyncDisposable(fn: () => Promise<void>): AsyncDisposable {
  let done = false
  return {
    async [Symbol.asyncDispose]() {
      if (done) return
      done = true
      await fn()
    },
  }
}

/** An `AbortController` that aborts when disposed. */
export function abortable(): AbortController & Disposable {
  const controller = new AbortController()
  return Object.assign(controller, {
    [Symbol.dispose]() {
      if (!controller.signal.aborted) controller.abort()
    },
  })
}
