import type { Context } from '../context/index.ts'

/** Brand used to tell an ElementNode apart from a plain attributes object. */
export const BRAND: unique symbol = Symbol('view.element')

export type Key = string | number

/** Anything that can appear in a child position. */
export type Child =
  | ElementNode
  | string
  | number
  | boolean
  | null
  | undefined
  | readonly Child[]

/** A view: what a component's yield ultimately describes. */
export type View = Child

/**
 * A render thunk. Yielding one parks the generator at that yield: the runtime
 * calls this in its place on every update, inside the tracking window a resume
 * would have opened.
 *
 * Synchronous by construction. An `async` thunk returns `Promise<View>`, which
 * is not a `View`, so it does not typecheck — and must not, since the tracking
 * window cannot span an `await` and a rejection cannot reach `gen.throw()`.
 */
export type ViewFn = () => View

/**
 * Everything a component may yield.
 *
 * Deliberately *not* `View`. A function is not a `Child` and must never become
 * one: the widening lives on the generator's `TYield` and nowhere else, so a
 * thunk stays unspellable in a child slot, in `portal()` and in `render()`.
 */
export type Yielded = View | ViewFn

export interface ElementNode<T = unknown> {
  readonly [BRAND]: true
  readonly tag: T
  readonly props: Readonly<Record<string, unknown>>
  readonly children: readonly Child[]
  readonly key: Key | undefined
}

// ---------------------------------------------------------------------------
// Generator component types
// ---------------------------------------------------------------------------

/**
 * A component generator.
 *
 * `yield` evaluates to nothing. Props do not travel through it — `props` is a
 * live object whose reads are tracked, so it is already current every time the
 * generator resumes and there is nothing to hand back.
 *
 * Leaving that slot empty is deliberate. Putting anything in it (Crank sends
 * the rendered DOM node) makes `TNext` depend on the component's own types,
 * which is what forced `component<Props>()` to take an explicit type argument
 * before — see `spikes/01-yield-typing.ts` for the mechanism. `ref` covers the
 * one case a return value would serve.
 *
 * `TYield` is a different slot, and widening it to `Yielded` is free: the union
 * is independent of `P`, so it is fully known before `P` is inferred.
 */
export type ComponentGen = Generator<Yielded, void, void>

export type AsyncComponentGen = AsyncGenerator<Yielded, void, void>

/**
 * A `yield*` delegate. Holds its own state, shares the host's lifecycle, and
 * its yields become the host's yields. `R` is what `yield*` evaluates to.
 */
export type Behavior<P = unknown, R = void> = Generator<Yielded, R, P>

export type AsyncBehavior<P = unknown, R = void> = AsyncGenerator<Yielded, R, P>

// ---------------------------------------------------------------------------
// Component context
// ---------------------------------------------------------------------------

/**
 * The component context, reached as `this` inside a component body.
 *
 *     const C = component(function* () {
 *       const theme = this.inject(Theme)
 *       yield () => div(theme.value)
 *     })
 *
 * It arrives through `this` rather than a second parameter so that a component
 * needing nothing from the runtime declares nothing, and one needing everything
 * declares nothing either. `this` is the language's own dynamic scope: it is
 * bound when the generator is *called*, so unlike an ambient module global it
 * stays correct for the whole body — including after an `await`, and inside any
 * arrow function the body closes over.
 *
 * The cost is that it does not cross a call boundary. A plain helper or a
 * `yield*` delegate has its own `this`, so it takes what it needs as an ordinary
 * argument — `useParams(this)` — or is invoked with `yield* behavior.call(this)`.
 * TypeScript rejects the form that would silently get `undefined`.
 */
export interface Ctx {
  /**
   * Resume this component explicitly. The signal layer is built on this; you
   * rarely need it directly.
   */
  refresh(): void

  /**
   * Aborted when this component unmounts. Pass to fetch, addEventListener, etc.
   *
   * The one piece of teardown that is not `using`: a signal has to be handed to
   * an API *before* the work starts, so there is nothing to bind a scope to.
   */
  readonly aborted: AbortSignal

  /**
   * Track signal reads that happen outside the automatic resume-to-yield
   * window — notably after an `await` in an async component.
   */
  track<T>(fn: () => T): T

  /**
   * Make a value available to this component's descendants.
   *
   * Call it before the first `yield`: it only reaches children mounted after
   * it, and everything a component mounts is mounted from its yields.
   */
  provide<T>(key: Context<T>, value: T): void

  /**
   * Read the value the nearest ancestor provided for `key`. Falls back to the
   * token's default, and throws when there is neither.
   */
  inject<T>(key: Context<T>): T
}

// ---------------------------------------------------------------------------
// Component definitions
// ---------------------------------------------------------------------------

export interface ComponentDef<P> {
  readonly render: (this: Ctx, props: P) => ComponentGen | AsyncComponentGen
  readonly name: string
  readonly isAsync: boolean
  /**
   * Skip a parent-driven re-render when the incoming props are shallow-equal to
   * the previous ones. On by default. Turn it off for a component whose output
   * depends on mutable state its props do not describe — but prefer moving that
   * state into a signal, which is tracked either way.
   */
  readonly memo: boolean
}

/** Components with no required props may be called with no arguments. */
export type PropsArg<P> = {} extends P
  ? [props?: P & { key?: Key }]
  : [props: P & { key?: Key }]

export interface ComponentFn<P> {
  (...args: PropsArg<P>): ElementNode<ComponentDef<P>>
  readonly def: ComponentDef<P>
  readonly displayName: string
}

// ---------------------------------------------------------------------------
// Attribute types, derived from lib.dom.d.ts
// ---------------------------------------------------------------------------

/** True iff X and Y are identical, including readonly modifiers. */
type IfEquals<X, Y, A, B> =
  (<T>() => T extends X ? 1 : 2) extends <T>() => T extends Y ? 1 : 2 ? A : B

type WritableKeys<T> = {
  [K in keyof T]-?: IfEquals<{ [Q in K]: T[K] }, { -readonly [Q in K]: T[K] }, K, never>
}[keyof T]

/**
 * Event handlers are `((ev) => any) | null` — a union, so they do not satisfy
 * `extends Function` and survive the method filter below. This is why handlers
 * arrive already typed per element at no cost.
 */
type HandlerKeys<T> = Extract<WritableKeys<T>, `on${string}`>

type DataKeys<T> = {
  [K in WritableKeys<T>]: K extends `on${string}`
    ? never
    : T[K] extends Function
      ? never
      : K
}[WritableKeys<T>]

type DomProps<K extends keyof HTMLElementTagNameMap> = Pick<
  HTMLElementTagNameMap[K],
  | DataKeys<HTMLElementTagNameMap[K]>
  | HandlerKeys<HTMLElementTagNameMap[K]>
>

export type StyleValue = string | Readonly<Record<string, string | number>>

interface CommonAttrs<E> {
  readonly [BRAND]?: never
  key?: Key
  /**
   * Runs on mount. Return a function to undo it on unmount.
   *
   * Typed as returning `void` rather than `void | (() => void)` on purpose: a
   * union of return types would reject the ordinary `ref: (el) => list.push(el)`,
   * because `push` hands back a number. A plain `void` return accepts any
   * expression body — TypeScript's rule for discarded return values — and the
   * cleanup is picked up at runtime by checking whether a function came back.
   */
  ref?: (el: E) => void
  class?: string
  style?: StyleValue
  dataset?: Readonly<Record<string, string | number | boolean>>
  /** Escape hatch for attributes with no DOM property (aria-*, custom). */
  attrs?: Readonly<Record<string, string | number | boolean | null | undefined>>
}

export type Attrs<K extends keyof HTMLElementTagNameMap> = Partial<
  Omit<DomProps<K>, 'style' | 'className' | 'classList' | 'dataset'>
> &
  CommonAttrs<HTMLElementTagNameMap[K]>

export interface TagFn<K extends keyof HTMLElementTagNameMap> {
  (...children: Child[]): ElementNode<K>
  (attrs: Attrs<K>, ...children: Child[]): ElementNode<K>
}

export type Tags = { [K in keyof HTMLElementTagNameMap]: TagFn<K> }
