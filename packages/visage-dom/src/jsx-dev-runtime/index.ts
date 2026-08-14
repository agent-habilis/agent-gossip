/**
 * Development entry point for the JSX transform.
 *
 * Transpilers emit `jsxDEV` instead of `jsx`/`jsxs` in development builds and
 * import it from `<jsxImportSource>/jsx-dev-runtime`, so this module has to
 * exist alongside `jsx-runtime` even though it adds nothing. The trailing
 * arguments carry source locations for error overlays; the runtime ignores them.
 */

import { build } from '../jsx-runtime/index.ts'
import type { ElementNode, Key } from '../types/index.ts'

export function jsxDEV(
  type: unknown,
  props: { children?: unknown; [prop: string]: unknown },
  key?: Key,
): ElementNode<unknown> {
  return build(type, props as Parameters<typeof build>[1], key)
}

export { Fragment, type JSX } from '../jsx-runtime/index.ts'
