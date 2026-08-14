import { expect, test } from 'bun:test'
import { component, render, signal, tags } from '../index.ts'
import { flushSync } from '../signal/index.ts'

const { div, span } = tags

function mulberry32(seed: number) {
  return () => {
    seed |= 0
    seed = (seed + 0x6d2b79f5) | 0
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

const Item = component<{ n: number }>(function* (props) {
  yield () => span(String(props.n))
})

/**
 * A row whose root is a fragment, so it owns two DOM nodes rather than one.
 * Odd-numbered ids use this, which puts multi-node fibers through the same
 * keyed matching and LIS ordering pass as single-node ones.
 */
const PairItem = component<{ n: number }>(function* (props) {
  yield () =>
    [span(String(props.n)), span(`${props.n}'`)]
})

const rowFor = (n: number) => (n % 2 === 1 ? PairItem({ key: n, n }) : Item({ key: n, n }))

/** How many DOM nodes the row for `n` owns. */
const widthOf = (n: number) => (n % 2 === 1 ? 2 : 1)

// Several seeds, because one seed only ever explores one path through the
// keyed matching and LIS ordering pass.
test.each([0xc0ffee, 0x1234abc, 0xfeed, 0x5eed])(
  'keyed reorder fuzz: order and node identity hold (seed %p)',
  (seed) => {
  const rng = mulberry32(seed)
  const list = signal<number[]>([])
  const App = component(function* () {
    yield () =>
      div({ id: 'root' }, list.value.map(rowFor))
  })

  const host = document.createElement('div')
  document.body.appendChild(host)
  render(App(), host)

  let nextId = 0
  const identity = new Map<number, Node[]>()

  const check = (label: string) => {
    const root = host.querySelector('#root')!
    const kids = [...root.childNodes]
    const expected = list.peek()
    const width = expected.reduce((sum, n) => sum + widthOf(n), 0)
    expect(kids.length, `${label}: node count`).toBe(width)

    // Walk the DOM in step with the data, consuming one or two nodes per row.
    let at = 0
    expected.forEach((n, i) => {
      const owned = kids.slice(at, at + widthOf(n)) as Element[]
      at += widthOf(n)
      const texts = owned.map((el) => el.textContent)
      const want = widthOf(n) === 2 ? [String(n), `${n}'`] : [String(n)]
      expect(texts, `${label}: row ${i} contents`).toEqual(want)

      const known = identity.get(n)
      if (known) {
        // Every node the row owns must be the same node as before, still
        // adjacent and still in order.
        known.forEach((node, j) => {
          expect(node === owned[j], `${label}: identity of ${n}[${j}]`).toBe(true)
        })
      } else {
        identity.set(n, owned)
      }
    })

    // anything gone from the list must not linger in the map's DOM
    for (const [n, nodes] of identity) {
      if (!expected.includes(n)) {
        for (const node of nodes) {
          expect(node.parentNode, `${label}: ${n} detached`).toBe(null)
        }
        identity.delete(n)
      }
    }
  }

  for (let round = 0; round < 400; round++) {
    const cur = [...list.peek()]
    const op = Math.floor(rng() * 8)
    let next = cur

    if (op === 0) next = [...cur].reverse()
    else if (op === 1) {
      next = [...cur]
      for (let i = next.length - 1; i > 0; i--) {
        const j = Math.floor(rng() * (i + 1))
        ;[next[i], next[j]] = [next[j]!, next[i]!]
      }
    } else if (op === 2) {
      next = [...cur]
      const count = Math.floor(rng() * 6)
      for (let i = 0; i < count; i++) {
        next.splice(Math.floor(rng() * (next.length + 1)), 0, nextId++)
      }
    } else if (op === 3) {
      next = [...cur]
      const count = Math.min(next.length, Math.floor(rng() * 6))
      for (let i = 0; i < count; i++) next.splice(Math.floor(rng() * next.length), 1)
    } else if (op === 4 && cur.length > 1) {
      next = [...cur]
      const a = Math.floor(rng() * next.length)
      const b = Math.floor(rng() * next.length)
      ;[next[a], next[b]] = [next[b]!, next[a]!]
    } else if (op === 5) {
      next = cur.length > 1 ? [...cur.slice(1), cur[0]!] : cur
    } else if (op === 6) next = []
    else next = Array.from({ length: Math.floor(rng() * 12) }, () => nextId++)

    list.value = next
    flushSync()
    check(`seed ${seed} round ${round} op ${op}`)
  }
  },
)

test('mixed keyed and unkeyed siblings survive reordering', () => {
  const list = signal<number[]>([1, 2, 3])
  const App = component(function* () {
    yield () =>
      div(
        { id: 'root' },
        span('head'),
        list.value.map((n) => Item({ key: n, n })),
        span('tail'),
      )
  })
  const host = document.createElement('div')
  document.body.appendChild(host)
  render(App(), host)

  const text = () => [...host.querySelector('#root')!.childNodes].map((n) => n.textContent)

  expect(text()).toEqual(['head', '1', '2', '3', 'tail'])
  list.value = [3, 1, 2]
  flushSync()
  expect(text()).toEqual(['head', '3', '1', '2', 'tail'])
  list.value = []
  flushSync()
  expect(text()).toEqual(['head', 'tail'])
  list.value = [9, 8]
  flushSync()
  expect(text()).toEqual(['head', '9', '8', 'tail'])
})
