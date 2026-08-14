import {
  BRAND,
  type Child,
  type ComponentDef,
  type ComponentFn,
  type ComponentGen,
  type AsyncComponentGen,
  type Ctx,
  type ElementNode,
  type Key,
} from '../types/index.ts'

export function isElement(value: unknown): value is ElementNode {
  return typeof value === 'object' && value !== null && BRAND in value
}

/** Distinguish a leading attributes object from a first child. */
function isAttrs(value: unknown): value is Record<string, unknown> {
  return (
    typeof value === 'object' &&
    value !== null &&
    !Array.isArray(value) &&
    !isElement(value)
  )
}

export function createElement<T>(
  tag: T,
  props: Record<string, unknown>,
  children: readonly Child[],
  key: Key | undefined = props['key'] as Key | undefined,
): ElementNode<T> {
  return {
    [BRAND]: true,
    tag,
    props,
    children,
    key,
  }
}

/** Shared stand-in for an element called with no attributes. Never mutated. */
export const NO_PROPS: Readonly<Record<string, unknown>> = Object.freeze({})

/**
 * Turn a variadic argument list into an element.
 *
 * The rest-args array is freshly allocated by the call and nobody else holds a
 * reference, so a leading attributes object is shifted off it in place and the
 * remainder becomes the child list — no second array, and no tuple to carry the
 * pair back to the caller. Element construction is the single hottest thing
 * this library does; a thousand-row render runs it eight thousand times.
 */
export function fromArgs<T>(tag: T, args: unknown[]): ElementNode<T> {
  if (args.length > 0 && isAttrs(args[0])) {
    const attrs = args[0] as Record<string, unknown>
    args.shift()
    return createElement(tag, attrs, args as Child[])
  }
  return createElement(tag, NO_PROPS as Record<string, unknown>, args as Child[])
}

// ---------------------------------------------------------------------------
// component()
// ---------------------------------------------------------------------------

/**
 * Define a component.
 *
 * The props type can be inferred from the parameter — `component(function*
 * (props: Props) {…})` — or given explicitly as `component<Props>(function*
 * (props) {…})`. Both work; pick whichever puts `Props` in the more readable
 * place.
 *
 * It was not always so. `yield` used to evaluate to the next props, which made
 * the generator's `TNext` depend on `P`; inference then had to resolve `P` from
 * the parameter and `TNext` from the body at the same time, and the two
 * conflicted. `TNext` is `void` now, so there is nothing left to conflict.
 * `spikes/01-yield-typing.ts` records the mechanism.
 *
 * `props` is live: read it inside the loop and the read is tracked. It is typed
 * `Readonly<P>`, though that only binds when `P` comes from the explicit type
 * argument — an annotation on the parameter wins over the contextual type. The
 * runtime guarantee is the one that always holds: writing to a prop throws.
 *
 * Calling the result builds a typed element descriptor rather than running the
 * generator, which is what gives the reconciler stable instance identity.
 */
export function component<P extends object = {}>(
  render: (this: Ctx, props: Readonly<P>) => ComponentGen,
  options?: ComponentOptions,
): ComponentFn<P> {
  return define(render, false, options)
}

/** Define an async component. See the tracking caveat in the README. */
export function asyncComponent<P extends object = {}>(
  render: (this: Ctx, props: Readonly<P>) => AsyncComponentGen,
  options?: ComponentOptions,
): ComponentFn<P> {
  return define(render, true, options)
}

export interface ComponentOptions {
  name?: string
  /**
   * Skip a parent-driven re-render when the incoming props are shallow-equal to
   * the previous ones. Defaults to `true`: signals already wake whatever read
   * them, so re-running a generator for identical props is pure cost. Set
   * `false` for a component that reads mutable state its props do not describe.
   */
  memo?: boolean
}

function define<P extends object>(
  render: (this: Ctx, props: P) => ComponentGen | AsyncComponentGen,
  isAsync: boolean,
  options: ComponentOptions | undefined,
): ComponentFn<P> {
  const name = options?.name ?? render.name
  const def: ComponentDef<P> = {
    render,
    name: name || 'Anonymous',
    isAsync,
    memo: options?.memo ?? true,
  }

  const fn = ((props?: P & { key?: Key }) =>
    createElement(def, (props ?? {}) as Record<string, unknown>, [])) as unknown as ComponentFn<P>

  Object.defineProperty(fn, 'def', { value: def })
  Object.defineProperty(fn, 'displayName', { value: def.name })
  Object.defineProperty(fn, 'name', { value: def.name })
  return fn
}

export function isComponentDef(tag: unknown): tag is ComponentDef<object> {
  return typeof tag === 'object' && tag !== null && 'render' in tag && 'isAsync' in tag
}

// ---------------------------------------------------------------------------
// Child helpers
// ---------------------------------------------------------------------------

/**
 * Flatten nested arrays and drop nothing — holes keep their position.
 *
 * The overwhelmingly common case is a child list with no nesting at all, so
 * that is checked for first and the input returned as-is. Callers only read the
 * result, and this runs on every element of every patch, so the copy is worth
 * avoiding.
 */
export function flatten(children: readonly Child[]): readonly Child[] {
  for (let i = 0; i < children.length; i++) {
    if (Array.isArray(children[i])) return flattenInto(children, [])
  }
  return children
}

function flattenInto(children: readonly Child[], out: Child[]): Child[] {
  for (const child of children) {
    if (Array.isArray(child)) flattenInto(child as readonly Child[], out)
    else out.push(child as Child)
  }
  return out
}

/** Attach keys to a mapped list. Prefer this over bare `.map()`. */
export function keyed<T>(
  items: readonly T[],
  getKey: (item: T, index: number) => Key,
  render: (item: T, index: number) => ElementNode,
): Child[] {
  return items.map((item, i) => {
    const node = render(item, i)
    // Rebuild rather than spread: a spread clones every field of every node in
    // the list on every render, which is exactly the cost a mapped list can
    // least afford.
    return createElement(node.tag, node.props, node.children, getKey(item, i))
  })
}
