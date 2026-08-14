/**
 * Scoped component styles, using nothing but the platform.
 *
 * `Style` builds a `<style>` element holding a prelude-less `@scope` block. The
 * browser scopes that block to the `<style>` element's own parent — so putting
 * one inside a component's root element scopes the rules to that component, and
 * the DOM position *is* the scope. There is no class, no hash, no attribute and
 * no registry, which means there is nothing to collide and nothing to clean up:
 *
 *     const ROW = css({ display: 'flex', gap: 8, '&:hover': { background: 'red' } })
 *
 *     const Row = component(function* (props) {
 *       yield () => li({}, Style(ROW), span({}, props.label))
 *     })
 *
 * Three properties fall out of it being an ordinary element rather than
 * machinery:
 *
 * - **Unmount is free.** The `<style>` is a child node, so the reconciler
 *   removes it with its component. No registry, no ref counting, no sheet GC.
 * - **Nesting resolves itself.** `@scope` adds *scoping proximity* to the
 *   cascade — nearest root wins, ahead of source order — so an inner
 *   component's own styles beat an outer one reaching in. Hash-based scoping
 *   (Vue, Svelte, Astro) cannot express that, which is why all three ship a
 *   `:global` escape hatch.
 * - **Duplicates are cheap.** Engines cache parsed stylesheets by source text,
 *   so a thousand rows carrying identical `<style>` elements is one parse. What
 *   a thousand rows do cost is a thousand DOM nodes; the elements are
 *   `display: none`, so they cost no layout.
 *
 * Hoist the style object with `css()`, as above. Compiled text is cached on its
 * identity, so a hoisted object compiles once per component type instead of once
 * per instance — and DEV warns when it sees the same CSS built twice. Writing
 * `Style({ … })` inline works and stays one step; it just recompiles.
 */

import { createElement } from 'visage-dom/element'
import type { ElementNode } from 'visage-dom/types'
import { compile } from './compile/index.ts'
import type { CheckStyle, StyleInput, StyleNode } from './values/index.ts'

export { raw } from './values/index.ts'
export type { Color, Declarations, Length, Raw, Size, Time, Token } from './values/index.ts'
export { tokens, Theme, type ThemeOptions, type TokenMap } from './tokens/index.ts'
export { configure, type Config } from './compile/index.ts'
export { caps, probe, type Caps } from './capabilities/index.ts'

const NO_PROPS: Readonly<Record<string, unknown>> = Object.freeze({})

/**
 * Build the scoped `<style>` element for a component.
 *
 * The argument is checked twice over. `StyleInput` types the properties it
 * knows — a declared property beats the index signature, so `padding: 'red'`
 * fails there. `CheckStyle` then walks what is left and rejects any unknown key
 * whose value is not a `raw()`, so `paddng: '8px'` fails too, while a nested
 * selector block passes through and is checked in turn.
 */
export function Style<const T extends StyleInput>(
  declarations: T & CheckStyle<T>,
): ElementNode<'style'> {
  return createElement('style', NO_PROPS as Record<string, unknown>, [
    compile(declarations as StyleNode),
  ])
}

/**
 * Declare a style object for hoisting, with the same checking `Style` applies.
 *
 * `Style({ … })` inline is complete on its own — this exists because hoisting is
 * worth doing and a plain `const` cannot be hoisted safely:
 *
 *     const CARD = { color: 'rgb(1, 2, 3)' }   // widens to `string`
 *     const CARD = css({ color: 'rgb(1, 2, 3)' })  // stays literal, and is checked
 *
 * TypeScript widens a string literal on assignment, so the const form loses
 * exactly the information the value grammar checks against, and `Color` would
 * reject its own documented usage. Passing through a `const` type parameter
 * keeps the literal.
 *
 * At runtime this returns its argument. What it buys is the compile cache:
 * compiled text is keyed on object identity, so a hoisted object compiles once
 * per component type where an inline literal compiles once per instance.
 */
export function css<const T extends StyleInput>(declarations: T & CheckStyle<T>): T {
  return declarations as T
}
