/** DOM node creation and property application. */

import type { Host, HostEl, HostNode } from '../host/index.ts'

const SVG_NS = 'http://www.w3.org/2000/svg'
const SVG_TAGS = new Set([
  'svg', 'circle', 'rect', 'line', 'path', 'g', 'text', 'polygon', 'polyline',
  'ellipse', 'defs', 'use', 'clipPath', 'mask', 'linearGradient', 'stop',
])

export function createNode(tag: string): Element {
  return SVG_TAGS.has(tag)
    ? document.createElementNS(SVG_NS, tag)
    : document.createElement(tag)
}

export function createText(value: string): Text {
  return document.createTextNode(value)
}

/** Placeholder for a null/false child, so siblings keep stable positions. */
export function createHole(): Comment {
  return document.createComment('')
}

/**
 * Properties where a bare number is not a length. `flex: 1` must stay `1`,
 * never `1px` — the latter collapses a flex item to a one-pixel basis.
 */
const UNITLESS = new Set([
  'animationIterationCount',
  'aspectRatio',
  'borderImageOutset',
  'borderImageSlice',
  'borderImageWidth',
  'columnCount',
  'flex',
  'flexGrow',
  'flexShrink',
  'fontWeight',
  'gridArea',
  'gridColumn',
  'gridColumnEnd',
  'gridColumnStart',
  'gridRow',
  'gridRowEnd',
  'gridRowStart',
  'lineHeight',
  'opacity',
  'order',
  'orphans',
  'scale',
  'tabSize',
  'widows',
  'zIndex',
  'zoom',
])

function cssNumber(key: string, value: number): string {
  if (value === 0 || UNITLESS.has(key) || key.startsWith('--')) return String(value)
  return `${value}px`
}

function applyStyle(el: HTMLElement, value: unknown, prev: unknown): void {
  if (typeof value === 'string') {
    el.style.cssText = value
    return
  }
  if (typeof prev === 'string' && typeof value === 'object') {
    el.style.cssText = ''
  }
  const next = (value ?? {}) as Record<string, string | number>
  const old = (typeof prev === 'object' && prev !== null ? prev : {}) as Record<
    string,
    string | number
  >
  for (const key of Object.keys(old)) {
    if (!(key in next)) el.style.removeProperty(hyphenate(key))
  }
  for (const [key, v] of Object.entries(next)) {
    if (old[key] === v) continue
    const prop = hyphenate(key)
    el.style.setProperty(prop, typeof v === 'number' ? cssNumber(key, v) : String(v))
  }
}

function hyphenate(key: string): string {
  return key.startsWith('--') ? key : key.replace(/[A-Z]/g, (m) => `-${m.toLowerCase()}`)
}

function applyDataset(el: HTMLElement, value: unknown, prev: unknown): void {
  const next = (value ?? {}) as Record<string, unknown>
  const old = (prev ?? {}) as Record<string, unknown>
  for (const key of Object.keys(old)) {
    if (!(key in next)) delete el.dataset[key]
  }
  for (const [key, v] of Object.entries(next)) {
    if (old[key] === v) continue
    el.dataset[key] = String(v)
  }
}

function applyAttrs(el: Element, value: unknown, prev: unknown): void {
  const next = (value ?? {}) as Record<string, unknown>
  const old = (prev ?? {}) as Record<string, unknown>
  for (const key of Object.keys(old)) {
    if (!(key in next)) el.removeAttribute(key)
  }
  for (const [key, v] of Object.entries(next)) {
    if (old[key] === v) continue
    if (v === null || v === undefined || v === false) el.removeAttribute(key)
    else el.setAttribute(key, v === true ? '' : String(v))
  }
}

const SKIP = new Set(['key', 'ref', 'children'])

/**
 * Diff one property. The type layer is built on DOM *properties*, so the
 * default path is a property assignment; `setAttribute` is the fallback for
 * names with no corresponding property.
 */
function setProp(el: Element, key: string, value: unknown, prev: unknown): void {
  if (SKIP.has(key)) return
  if (Object.is(value, prev)) return

  switch (key) {
    case 'class':
      if (value === null || value === undefined) el.removeAttribute('class')
      // `className` is a plain string property on HTML elements and setting it
      // is measurably cheaper than going through the attribute map. On SVG it
      // is a read-only `SVGAnimatedString`, so those keep the attribute path.
      else if (el instanceof HTMLElement) el.className = String(value)
      else el.setAttribute('class', String(value))
      return
    case 'style':
      applyStyle(el as HTMLElement, value, prev)
      return
    case 'dataset':
      applyDataset(el as HTMLElement, value, prev)
      return
    case 'attrs':
      applyAttrs(el, value, prev)
      return
    case 'for':
      ;(el as HTMLLabelElement).htmlFor = String(value ?? '')
      return
  }

  // Event handlers: direct property assignment replaces cleanly, so there is
  // no listener bookkeeping and handler identity never needs memoizing.
  if (key.startsWith('on')) {
    ;(el as unknown as Record<string, unknown>)[key] =
      typeof value === 'function' ? value : null
    return
  }

  if (assignable(el, key)) {
    try {
      ;(el as unknown as Record<string, unknown>)[key] = value
      return
    } catch {
      // Read-only property; fall through to setAttribute.
      ASSIGNABLE.get(el.tagName)?.set(key, false)
    }
  }

  if (value === null || value === undefined || value === false) el.removeAttribute(key)
  else el.setAttribute(key, value === true ? '' : String(value))
}

/**
 * Whether `key` names a property on this tag rather than an attribute.
 *
 * `key in el` walks the prototype chain, and the answer only ever depends on
 * the tag, so it is worth remembering. A per-tag map keeps custom elements from
 * poisoning each other's answers.
 */
const ASSIGNABLE = new Map<string, Map<string, boolean>>()

function assignable(el: Element, key: string): boolean {
  let perTag = ASSIGNABLE.get(el.tagName)
  if (perTag === undefined) {
    perTag = new Map()
    ASSIGNABLE.set(el.tagName, perTag)
  }
  let known = perTag.get(key)
  if (known === undefined) {
    known = key in el
    perTag.set(key, known)
  }
  return known
}

export function applyProps(
  el: Element,
  next: Readonly<Record<string, unknown>>,
  prev?: Readonly<Record<string, unknown>>,
): void {
  if (prev === undefined) {
    for (const key in next) setProp(el, key, next[key], undefined)
    return
  }
  if (prev === next) return

  // `for...in` rather than `Object.keys`: this runs on every element of every
  // patch, and the two key arrays would be pure garbage.
  for (const key in prev) {
    if (!(key in next)) setProp(el, key, undefined, prev[key])
  }
  for (const key in next) {
    setProp(el, key, next[key], prev[key])
  }
}

export function runRef(
  props: Readonly<Record<string, unknown>>,
  el: Element,
): (() => void) | void {
  const ref = props['ref']
  if (typeof ref !== 'function') return
  const detach = (ref as (e: Element) => unknown)(el)
  if (typeof detach === 'function') return detach as () => void
}

/**
 * The DOM as a reconciler host.
 *
 * Every method is a one-line forward to the function above it or to a DOM call
 * the reconciler used to make inline. The casts are the price of `HostNode`
 * being opaque, and they are all safe by construction: nothing but this module
 * ever creates the nodes that reach these methods.
 *
 * No `commit` — a DOM write *is* the commit, so there is nothing to do at the
 * end of one and the property is left off rather than defined as a no-op.
 */
export const domHost: Host = {
  // Assigned rather than wrapped. A method slot's parameters are bivariant, so
  // a function taking `Element` satisfies one declared over `HostEl` — which
  // matters here and not elsewhere: `applyProps` runs once per element on every
  // patch, and a wrapper frame around it is measurable at 10 000 rows.
  createNode,
  createText,
  createHole,
  applyProps,
  runRef,

  setText(node: HostNode, value: string): void {
    ;(node as Text).data = value
  },

  insert(parent: HostEl, node: HostNode, before: HostNode | null): void {
    ;(parent as Element).insertBefore(node as Node, before as Node | null)
  },
  remove(node: HostNode): void {
    const dom = node as Node
    dom.parentNode?.removeChild(dom)
  },
  append(parent: HostEl, node: HostNode): void {
    ;(parent as Element).appendChild(node as Node)
  },

  nextSibling(node: HostNode): HostNode | null {
    return (node as Node).nextSibling
  },
  childCount(parent: HostEl): number {
    return (parent as Element).childNodes.length
  },
  clear(parent: HostEl): void {
    ;(parent as Element).textContent = ''
  },

  createFragment(): HostEl {
    return document.createDocumentFragment()
  },

  supportsPortals: true,
}
