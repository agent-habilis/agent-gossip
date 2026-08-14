/**
 * The optional JSX front end.
 *
 * The thing worth testing here is that JSX is a *front end* and not a second
 * element model: it must produce the same `ElementNode`s the `tags` proxy does,
 * and drive the same reconciler. Most of the deliverable is types, so the
 * `@ts-expect-error` cases at the bottom carry as much weight as the runtime
 * assertions — they are what proves per-element typing survived the move.
 */

import { test, expect, beforeEach } from 'bun:test'
import { component, render, signal, tags, flushSync } from '../index.ts'
import { Fragment } from '../jsx-runtime/index.ts'
import type { Child, ComponentFn, ElementNode } from '../index.ts'

const { div, span } = tags

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.appendChild(host)
})

/** ElementNode's fields are readonly and the brand is a symbol; peek generically. */
function node(value: unknown): { tag: unknown; props: Record<string, unknown>; children: unknown[]; key: unknown } {
  return value as never
}

// ---------------------------------------------------------------------------
// Equivalence with the tags proxy
// ---------------------------------------------------------------------------

test('produces the same element node as the tags proxy', () => {
  const viaTags = node(div({ class: 'row' }, 'hi'))
  const viaJsx = node(<div class="row">hi</div>)

  expect(viaJsx.tag).toBe(viaTags.tag)
  expect(viaJsx.props).toEqual(viaTags.props)
  expect(viaJsx.children).toEqual(viaTags.children)
  expect(viaJsx.key).toBe(viaTags.key)
})

test('renders identical DOM through both front ends', () => {
  const a = document.createElement('div')
  const b = document.createElement('div')
  render(div({ class: 'row' }, span('hi')), a)
  render(<div class="row"><span>hi</span></div>, b)
  expect(b.innerHTML).toBe(a.innerHTML)
})

test('normalises a single child and a children array the same way', () => {
  expect(node(<div>one</div>).children).toEqual(['one'])
  expect(node(<div>{'one'}{'two'}</div>).children).toEqual(['one', 'two'])
  expect(node(<div />).children).toEqual([])
})

// ---------------------------------------------------------------------------
// Components — the adapter that fails silently if it is wrong
// ---------------------------------------------------------------------------

interface ItemProps {
  label: string
}

const Item: ComponentFn<ItemProps> = component<ItemProps>(function* (props) {
  yield () => <li class="item">{props.label}</li>
})

test('unwraps a ComponentFn to its def, as calling it would', () => {
  // The reconciler matches instances on def identity (fiber.def === vnode.tag).
  // Miss this and a component mounts as an unknown host element instead — the
  // DOM looks wrong but nothing throws.
  const def = (Item as unknown as { def: unknown }).def
  expect(node(<Item label="x" />).tag).toBe(def)
  expect(node(Item({ label: 'x' })).tag).toBe(def)
})

test('mounts a component written in JSX', () => {
  render(<ul><Item label="one" /><Item label="two" /></ul>, host)
  expect(host.innerHTML).toBe('<ul><li class="item">one</li><li class="item">two</li></ul>')
})

test('delivers JSX children to a component through props', () => {
  // A comp fiber never reads node.children, so children have to stay in props.
  // Move them to the children array and they vanish without an error.
  const Panel = component<{ children?: unknown }>(function* (props) {
    yield () => <section>{props.children as never}</section>
  })

  render(<Panel><b>bold</b> and text</Panel>, host)
  expect(host.innerHTML).toBe('<section><b>bold</b> and text</section>')
})

test('leaves a component node with an empty children array', () => {
  const Box = component<{ children?: Child }>(function* (props) {
    yield () => <div>{props.children}</div>
  })

  const built = node(<Box>inner</Box>)
  expect(built.children).toEqual([])
  expect((built.props as { children?: unknown }).children).toBe('inner')
})

test('rejects children on a component that does not declare them', () => {
  // Falls out of ElementChildrenAttribute: children are checked like any other
  // prop, so a component opts in by declaring `children` in its props type.
  // @ts-expect-error ItemProps has no children
  const wrong = <Item label="x">nope</Item>
  expect(wrong).toBeTruthy()
})

test('keeps memo working for a childless component', () => {
  // props.children is a fresh array every render, so a component that takes
  // children can never bail out. One that does not, must still be able to.
  let renders = 0
  const Counted = component<{ label: string }>(function* (props) {
    while (true) {
      renders++
      yield <i>{props.label}</i>
    }
  })

  const root = render(<Counted label="same" />, host)
  expect(renders).toBe(1)
  root.update(<Counted label="same" />)
  flushSync()
  expect(renders).toBe(1)
  root.update(<Counted label="changed" />)
  flushSync()
  expect(renders).toBe(2)
})

test('drives the same reactive path as the tags proxy', () => {
  const App = component(function* () {
    const n = signal(0)
    const inc = () => n.value++
    yield () => (
      <div>
        <button onclick={inc}>+1</button>
        <span>{`count ${n.value}`}</span>
      </div>
    )
  })

  render(<App />, host)
  expect(host.querySelector('span')!.textContent).toBe('count 0')

  host.querySelector('button')!.click()
  flushSync()
  expect(host.querySelector('span')!.textContent).toBe('count 1')
})

// ---------------------------------------------------------------------------
// key — passed as the transform's third argument, not in props
// ---------------------------------------------------------------------------

test('lifts key out of the third argument onto the node', () => {
  expect(node(<div key="k7" />).key).toBe('k7')
  expect(node(<Item key={3} label="x" />).key).toBe(3)
})

test('keeps key off the DOM', () => {
  render(<div key="k7" class="row" />, host)
  const el = host.firstElementChild!
  expect(el.hasAttribute('key')).toBe(false)
  expect(el.getAttribute('class')).toBe('row')
})

test('preserves identity across a keyed reorder', () => {
  const List = component<{ ids: number[] }>(function* (props) {
    yield () =>
      <ul>{props.ids.map((id) => <li key={id}>{id}</li>)}</ul>
  })

  const root = render(<List ids={[1, 2, 3]} />, host)
  const first = host.querySelectorAll('li')[0]!
  root.update(<List ids={[3, 2, 1]} />)
  flushSync()

  expect([...host.querySelectorAll('li')].map((li) => li.textContent)).toEqual(['3', '2', '1'])
  // Same node moved, not rebuilt.
  expect(host.querySelectorAll('li')[2]).toBe(first)
})

// ---------------------------------------------------------------------------
// Fragments
// ---------------------------------------------------------------------------

test('renders a fragment inline in a child position', () => {
  render(
    <div>
      <>
        <span class="a">one</span>
        <span class="b">two</span>
      </>
    </div>,
    host,
  )
  // No wrapper element and no marker node: the children are spliced in.
  expect(host.innerHTML).toBe('<div><span class="a">one</span><span class="b">two</span></div>')
})

test('exports Fragment as a symbol', () => {
  expect(typeof Fragment).toBe('symbol')
})

test('allows a fragment as a component root', () => {
  const Multi = component(function* () {
    yield () => (<><span>a</span><span>b</span></>) as unknown as ElementNode
  })
  render(<Multi />, host)
  // Spliced straight into the parent: no wrapper, no marker node.
  expect(host.innerHTML).toBe('<span>a</span><span>b</span>')
})

// ---------------------------------------------------------------------------
// Types — these assertions are compile-time
// ---------------------------------------------------------------------------

test('keeps per-element typing from Attrs<K>', () => {
  // Handlers arrive typed per element, straight out of HTMLElementTagNameMap.
  const withHandler = <a onclick={(event) => String(event.clientX)} href="/x" />
  expect(node(withHandler).tag).toBe('a')

  // @ts-expect-error href is not a property of HTMLSpanElement
  const wrongElement = <span href="/x" />
  // @ts-expect-error onclick provides a MouseEvent, not a string
  const wrongHandler = <a onclick={(event) => event.toUpperCase()} />
  // @ts-expect-error href must be a string
  const wrongType = <a href={42} />

  expect([wrongElement, wrongHandler, wrongType].every(Boolean)).toBe(true)
})

test('checks component props', () => {
  // @ts-expect-error label is required
  const missing = <Item />
  // @ts-expect-error label must be a string
  const wrongType = <Item label={42} />

  expect([missing, wrongType].every(Boolean)).toBe(true)
})
