import { createElement, fromArgs } from '../element/index.ts'
import type { Child, ElementNode, Tags } from '../types/index.ts'

const cache = new Map<string, (...args: unknown[]) => ElementNode<string>>()

function makeTag(name: string) {
  const existing = cache.get(name)
  if (existing) return existing
  const fn = (...args: unknown[]): ElementNode<string> => fromArgs(name, args)
  cache.set(name, fn)
  return fn
}

/**
 * Every HTML tag as a typed factory function. Typed as a mapped type over
 * `HTMLElementTagNameMap`, implemented as a Proxy — so it is exhaustive and
 * fully checked without listing a single tag by hand.
 *
 *     const { div, button, span } = tags
 *     div({ class: 'row' }, button({ onclick: inc }, 'inc'), span('hi'))
 */
export const tags: Tags = new Proxy(Object.create(null) as Tags, {
  get(_target, prop: string | symbol) {
    if (typeof prop !== 'string') return undefined
    return makeTag(prop)
  },
  has() {
    return true
  },
})

/** Build a factory for a tag not in `HTMLElementTagNameMap` (custom elements). */
export function tag(name: string) {
  return (...args: unknown[]): ElementNode<string> => fromArgs(name, args)
}

/** SVG elements need a namespace; `svg.circle(...)` marks them for the renderer. */
const SVG_NS = 'http://www.w3.org/2000/svg'

export const svg: Record<string, (...args: unknown[]) => ElementNode<string>> = new Proxy(
  Object.create(null) as Record<string, (...args: unknown[]) => ElementNode<string>>,
  {
    get(_t, prop: string | symbol) {
      if (typeof prop !== 'string') return undefined
      return (...args: unknown[]): ElementNode<string> => {
        const node = fromArgs(prop, args)
        return createElement(prop, { ...node.props, xmlns: SVG_NS }, node.children)
      }
    },
  },
)

export type { Child }
