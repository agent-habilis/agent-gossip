/**
 * The DOM host bound to the reconciler.
 *
 * Its own module rather than a function in `../reconcile/index.ts`, because
 * that import would pin `domHost` into every bundle reaching the reconciler —
 * including the canvas and terminal backends, which have no DOM. And its own
 * module rather than a function in `./index.ts`, because that would make the
 * host implementation depend on the reconciler and cost `domHost` the property
 * of being importable on its own.
 *
 * So the arrow runs one way only: `dom/render.ts` → `reconcile` and `dom`,
 * neither of which knows about the other.
 */

import { renderWith, type Root } from '../reconcile/index.ts'
import type { View } from '../types/index.ts'
import { domHost } from './index.ts'

export function render(view: View, container: Element): Root {
  return renderWith(view, container, domHost)
}
