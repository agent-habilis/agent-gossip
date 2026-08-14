/**
 * Optional JSX front end.
 *
 * This module is a leaf: it imports from `element.ts` and `types.ts`, and
 * nothing in the library imports it back. `src/index.ts` in particular does not
 * re-export `Fragment`, so a consumer who never writes JSX never pulls this
 * file into their module graph. `test/optional.test.ts` enforces that.
 *
 * There is no second element model here — JSX compiles to the same
 * `createElement` calls `tags` makes, so both front ends produce identical
 * `ElementNode`s and drive the same reconciler.
 *
 * Enable per project with:
 *
 *     "jsx": "react-jsx",
 *     "jsxImportSource": "visage"
 *
 * Those settings apply only to `.tsx` files, which is what makes the layer
 * opt-in by file extension.
 */

import { createElement } from '../element/index.ts'
import type { Attrs, Child, ComponentFn, ElementNode, Key } from '../types/index.ts'

/**
 * `<>…</>`. Children pass straight through as an array, which the reconciler
 * flattens in any child position (`flatten()` in element.ts) and mounts as a
 * fragment fiber when it is a component's whole root view.
 */
export const Fragment: unique symbol = Symbol.for('view.fragment')

interface JsxProps {
  children?: Child | Child[]
  [prop: string]: unknown
}

/**
 * Shared by `jsx`, `jsxs` and `jsxDEV`. The three differ only in arguments the
 * runtime does not need: `jsxs` signals a statically-known children array, and
 * `jsxDEV` adds source-location debug parameters.
 */
const EMPTY: readonly Child[] = Object.freeze([])

function toChildren(children: Child | Child[] | undefined): readonly Child[] {
  // A single child arrives bare rather than wrapped in an array.
  if (children === undefined) return EMPTY
  return Array.isArray(children) ? children : [children]
}

export function build(type: unknown, props: JsxProps, key?: Key): ElementNode<unknown> {
  if (type === Fragment) {
    // An array is a valid Child, so this is honest at runtime; the cast exists
    // only because JSX.Element has to name one type for every expression.
    return toChildren(props.children) as unknown as ElementNode<unknown>
  }

  if (typeof type === 'function') {
    // Calling a ComponentFn unwraps to its def, and the reconciler matches
    // instances on def identity (`fiber.def === vnode.tag`, reconcile.ts). JSX
    // hands us the function instead, so do the same unwrapping here — miss it and
    // components mount as unknown host elements without throwing.
    if ('def' in type) {
      // Children stay in props on purpose. A comp fiber never reads
      // `node.children`, so anything moved there would silently vanish; leaving
      // it in props is what makes `props.children` work and is the only route a
      // JSX child has into a component.
      return createElement((type as ComponentFn<object>).def, props, EMPTY, key)
    }

    // Stateless view function (no `component()` wrapper): invoke now so the
    // parent's thunk re-evaluates it on each render, matching call-site helpers
    // like `Text({…})`. Without this, JSX would treat the function as a host
    // tag and `createNode` would stringify it into a broken element.
    const view = (type as (props: JsxProps) => unknown)(props)
    if (
      key !== undefined &&
      typeof view === 'object' &&
      view !== null &&
      'tag' in view &&
      (view as ElementNode<unknown>).key === undefined
    ) {
      const node = view as ElementNode<unknown>
      return createElement(node.tag, node.props, node.children, key)
    }
    return view as ElementNode<unknown>
  }

  // Host element: children have to move out of props and into the children
  // array, or they are never mounted.
  if (!('children' in props)) {
    // No copy needed, and the props object is fresh from the transform anyway.
    return createElement(type, props, EMPTY, key)
  }
  const { children, ...rest } = props
  return createElement(type, rest, toChildren(children), key)
}

export const jsx = build
export const jsxs = build

/**
 * The JSX namespace has to be exported from the module named by
 * `jsxImportSource`, which is why it lives here rather than in types.ts.
 */
export declare namespace JSX {
  type Element = ElementNode<unknown>

  /** Tells TypeScript which prop carries children. */
  interface ElementChildrenAttribute {
    children: {}
  }

  /** Accepted on every element and component, on top of its own props. */
  interface IntrinsicAttributes {
    key?: Key
  }

  /**
   * The whole reason this layer is cheap: `Attrs<K>` is already derived from
   * `HTMLElementTagNameMap`, so per-element checking and typed event handlers
   * carry over unchanged. `<a onclick={e => e.clientX}/>` still gets MouseEvent.
   */
  type IntrinsicElements = {
    [K in keyof HTMLElementTagNameMap]: Attrs<K> & { children?: Child | Child[] }
  }
}

export type { ComponentFn }
