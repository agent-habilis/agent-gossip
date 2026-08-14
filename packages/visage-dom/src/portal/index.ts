/**
 * Render part of a view into an element outside the current subtree.
 *
 * Modals, tooltips, toasts and popovers all want to escape whatever `overflow`
 * or `z-index` context they were declared in, without moving where they live in
 * the code.
 *
 *     portal(document.body, div({ class: 'modal' }, 'hi'))
 *
 * A factory rather than a component, for the same reason context is not a
 * provider element: `component()` builds descriptors with an empty children
 * array, so a component cannot take positional children and a wrapper would
 * only read well in JSX.
 */

import { createElement } from '../element/index.ts'
import type { Child, ElementNode } from '../types/index.ts'

/** Marks a descriptor as a portal. A symbol, so no tag name can collide. */
export const PORTAL: unique symbol = Symbol('view.portal')

export function portal(target: Element, ...children: Child[]): ElementNode<typeof PORTAL> {
  return createElement(PORTAL, { target }, children)
}

export function isPortal(tag: unknown): tag is typeof PORTAL {
  return tag === PORTAL
}
