import { beforeEach, expect, test } from 'bun:test'
import { flushSync, render } from 'visage-dom'
import { Table } from './Table.tsx'
import type { Column } from './Table.tsx'

interface Deployment {
  id: string
  branch: string
  age: string
}

const deployments: Deployment[] = [
  { id: 'dpl_1', branch: 'main', age: '2m' },
  { id: 'dpl_2', branch: 'feat/grid', age: '5m' },
]

const columns: Column<Deployment>[] = [
  { key: 'branch', header: 'Branch' },
  { key: 'age', header: 'Age', width: 5, align: 'right' },
]

const rowKey = (row: Deployment): string => row.id

let host: HTMLElement

beforeEach(() => {
  document.body.innerHTML = ''
  host = document.createElement('div')
  document.body.append(host)
})

/** Rendered text, excluding any scoped `<style>` element's own contents. */
const text = (el: Element): string =>
  [...el.childNodes]
    .filter((n) => n.nodeName !== 'STYLE')
    .map((n) => n.textContent)
    .join('')

test('renders header cells and body cells in grid order', () => {
  render(<Table columns={columns} rows={deployments} rowKey={rowKey} />, host)

  expect([...host.querySelectorAll('[role="columnheader"]')].map(text)).toEqual(['Branch', 'Age'])
  // Row-major, because grid order is document order — there is no row element.
  expect([...host.querySelectorAll('[role="cell"]')].map(text)).toEqual([
    'main',
    '2m',
    'feat/grid',
    '5m',
  ])
})

test('the column widths reach the grid as a custom property', () => {
  render(<Table columns={columns} rows={deployments} rowKey={rowKey} />, host)
  const root = host.firstElementChild as HTMLElement

  expect(root.style.getPropertyValue('--ms-table-columns')).toBe('minmax(0, 1fr) 5ch')
  // The template varies per table; the rule that reads it does not.
  expect(host.querySelector('style')!.textContent).toContain(
    'grid-template-columns:var(--ms-table-columns)',
  )
})

test('a column render function replaces the default cell text', () => {
  const rendered: Column<Deployment>[] = [
    { key: 'branch', header: 'Branch', render: (row) => <span>{row.branch.toUpperCase()}</span> },
  ]
  render(<Table columns={rendered} rows={deployments} rowKey={rowKey} />, host)

  expect([...host.querySelectorAll('[role="cell"] span')].map((el) => el.textContent)).toEqual([
    'MAIN',
    'FEAT/GRID',
  ])
})

test('composite keys keep cells with their rows across a reorder', () => {
  const root = render(<Table columns={columns} rows={deployments} rowKey={rowKey} />, host)
  const before = [...host.querySelectorAll('[role="cell"]')]

  root.update(<Table columns={columns} rows={[...deployments].reverse()} rowKey={rowKey} />)
  flushSync()
  const after = [...host.querySelectorAll('[role="cell"]')]

  // The same DOM nodes, moved rather than rewritten: the second row's two cells
  // are now the first two. This is what the keyed <Fragment> used to guarantee.
  expect(after[0]).toBe(before[2]!)
  expect(after[1]).toBe(before[3]!)
  expect(after.map(text)).toEqual(['feat/grid', '5m', 'main', '2m'])
})

test('rules are keyed per row and hidden from assistive technology', () => {
  render(
    <Table columns={columns} rows={deployments} rowKey={rowKey} rowRules caption="Deployments" />,
    host,
  )

  // One under the header, one between the two rows — a rule costs a row, so
  // there is never one above the first body row or below the last.
  const rules = [...host.querySelectorAll('[data-ms-rule]')]
  expect(rules).toHaveLength(2)
  expect(rules.every((el) => el.getAttribute('aria-hidden') === 'true')).toBe(true)
  expect(text(host.querySelector('[data-ms-caption]')!)).toBe('Deployments')
})

test('one stylesheet for the whole table, whatever the row count', () => {
  const many: Deployment[] = Array.from({ length: 50 }, (_, i) => ({
    id: `dpl_${i}`,
    branch: `branch-${i}`,
    age: '1m',
  }))
  render(<Table columns={columns} rows={many} rowKey={rowKey} />, host)

  // A <style> per cell would be a hundred extra nodes here and three thousand on
  // a real table; descendant selectors from one scope root cost exactly one.
  expect(host.querySelectorAll('[role="cell"]')).toHaveLength(100)
  expect(host.querySelectorAll('style')).toHaveLength(1)
})
