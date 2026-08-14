import { component } from 'visage-dom'
import type { Attrs, Child, ComponentDef, ElementNode, Key } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { withStyle } from '../../internal.ts'
import { ONE_ROW, SNAP, rows } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export interface Column<T> {
  /** Property key used for the default cell render, and half of the cell's key. */
  key: keyof T & string
  header: Child
  /**
   * Width in cells. Omit for exactly one flexible column that absorbs the
   * remaining space — see the note on `Table` about why only one.
   */
  width?: number
  align?: 'left' | 'right'
  render?: (row: T) => Child
}

export interface TableProps<T> extends Attrs<'div'> {
  columns: Column<T>[]
  rows: T[]
  /** Extracts a stable key from a row. The other half of every cell's key. */
  rowKey: (row: T) => Key
  /** Draw a rule under the header. */
  headerRule?: boolean
  /** Draw a rule between every body row. Costs one row per rule. */
  rowRules?: boolean
  caption?: string
}

/**
 * One cell, header or body.
 *
 * `minWidth: 0` is what lets the flexible column narrow at all: a grid item's
 * automatic minimum is its content size, so without it a long branch name would
 * refuse to shrink and push every column to its right off the lattice.
 */
const CELL = {
  ...ONE_ROW,
  overflow: 'hidden',
  whiteSpace: 'nowrap',
  minWidth: 0,
} as const

/**
 * The whole table's rules, in one stylesheet at the root, selecting descendants.
 *
 * A deliberate departure from one `Style()` per component, and the row count is
 * the reason. A five-hundred-row table with six columns is three thousand cells;
 * a `<style>` inside each is three thousand extra DOM nodes to create, insert
 * and eventually remove, for text the engine has already parsed once. One
 * stylesheet at the scope root costs a single node no matter how large the
 * table, and `@scope` still keeps its rules from reaching anything outside it.
 *
 * The hooks are the roles the cells already carry for accessibility rather than
 * styling-only attributes, on the same reasoning as `aria-selected` in Tabs: the
 * attribute has to be right anyway, so a second one mirroring it is a second
 * thing to keep in step. `data-ms-rule` and `data-ms-caption` are the exceptions
 * — those elements have no role to select on, and `[aria-hidden]` would also
 * match anything a caller hides inside a cell.
 */
const TABLE = css({
  '[data-ms-caption]': {
    ...ONE_ROW,
    color: t.fgSubtle,
  },

  '[role="table"]': {
    display: 'grid',
    /*
     * The one declaration that varies per instance, so it cannot live in this
     * object — a template built per render would compile a fresh stylesheet per
     * table, which is the runtime-CSS failure mode scoped static rules exist to
     * avoid. The component sets `--ms-table-columns` on the inline style and
     * this reads it, so the varying part is a value rather than a rule.
     */
    gridTemplateColumns: raw('var(--ms-table-columns)'),
    /*
     * Column gap is a whole cell, matching the single space a terminal would put
     * between columns. `align-items: center` is not needed — every cell is
     * exactly one row tall, so content is already on the baseline.
     */
    columnGap: '1ch',
    ...SNAP,
  },

  '[role="columnheader"]': {
    ...CELL,
    color: t.fgMuted,
    textTransform: 'uppercase',
  },
  '[role="cell"]': CELL,

  // Equal specificity with the two blocks above, so source order is what puts it
  // last and lets it win. Left is the default and needs no rule.
  '[data-ms-align="right"]': { textAlign: 'right' },

  /** Spans every column, so a rule runs the full width of the table. */
  '[data-ms-rule]': {
    gridColumn: '1 / -1',
    display: 'flex',
    alignItems: 'center',
    height: rows(1),
    '&::before': {
      content: "''",
      // `flex: 1` would serialise as `1px`: a bare number is a length for every
      // property the table does not mark unitless, and `flex` is a shorthand.
      flex: '1',
      borderTop: `${t.msBorderWidth} solid ${t.border}`,
    },
  },
})

/**
 * The row type the implementation is compiled at. See the note on `Table` for
 * why the runtime is monomorphic and the export is not.
 */
type AnyRow = Record<string, unknown>

/** One grid cell. `key` is composite — see the note in the render loop. */
const cell = (
  role: 'cell' | 'columnheader',
  column: Column<AnyRow>,
  content: Child,
  key: Key,
): ElementNode<unknown> => (
  <div key={key} attrs={{ role }} dataset={{ msAlign: column.align ?? 'left' }}>
    {content}
  </div>
)

/** A full-width rule. Hidden from assistive technology: it is a separator, not data. */
const rule = (key: Key): ElementNode<unknown> => (
  <div key={key} dataset={{ msRule: '' }} attrs={{ 'aria-hidden': 'true' }} />
)

const TableImpl = component<TableProps<AnyRow>>(function* (props) {
  yield () => {
    const {
      columns,
      rows,
      rowKey,
      headerRule = true,
      rowRules = false,
      caption,
      style,
      ...rest
    } = props

    /*
     * `minmax(0, 1fr)` rather than `1fr`, because `1fr`'s floor is its content —
     * the same automatic minimum `CELL` overrides, one level up.
     */
    const template = columns
      .map((column) => (column.width !== undefined ? `${column.width}ch` : 'minmax(0, 1fr)'))
      .join(' ')

    /*
     * Flat, with composite keys.
     *
     * React grouped each row's optional rule and N cells in a keyed
     * `<Fragment>`. visage has no such thing to key — `keyed()` requires its
     * render to return a single `ElementNode` — so the grouping is dissolved and
     * the identity it carried moves into the keys themselves:
     * `${rowKey}:${column.key}` for a cell and `r:${rowKey}` for a rule. That
     * preserves what the keyed Fragment guaranteed: reorder the rows and the
     * reconciler moves each row's own cells with it, rather than rewriting text
     * into whichever cell happens to sit at that position.
     */
    const items: Child[] = []

    for (const column of columns) {
      items.push(cell('columnheader', column, column.header, `h:${column.key}`))
    }
    if (headerRule) items.push(rule('h'))

    rows.forEach((row, index) => {
      const key = rowKey(row)
      if (rowRules && index > 0) items.push(rule(`r:${key}`))
      for (const column of columns) {
        const content = column.render ? column.render(row) : String(row[column.key] ?? '')
        items.push(cell('cell', column, content, `${key}:${column.key}`))
      }
    })

    return (
      <div {...rest} style={withStyle(style, { '--ms-table-columns': template })}>
        {Style(TABLE)}
        {caption != null && <div dataset={{ msCaption: '' }}>{caption}</div>}
        {/* Custom properties inherit, so the grid reads the template the root sets. */}
        <div attrs={{ role: 'table' }}>{items}</div>
      </div>
    )
  }
})

/**
 * A data table laid out on the character grid.
 *
 * Built on CSS Grid rather than `<table>` because a real table's column
 * algorithm sizes to content in fractional pixels, which is the one thing this
 * system cannot allow. Declaring widths in cells means the browser and a
 * terminal compute the same layout.
 *
 * At most one column should omit `width`. That column takes the slack; give two
 * columns slack and they split a remainder that is not guaranteed to divide into
 * whole cells, putting everything to their right off the lattice.
 *
 * ## Generic at the call site, monomorphic at runtime
 *
 * `component<P>` returns a `ComponentFn<P>` — one concrete props type — so
 * `Table<T>` cannot stay generic through it, and it has to go through it: the
 * jsx-runtime only unwraps a function carrying `.def`, and a plain generic
 * function in a JSX position renders its own source text as a tag name.
 *
 * So the implementation compiles at `Record<string, unknown>`, which is all it
 * needs — it reads `row[column.key]` and calls `rowKey(row)`, and neither cares
 * what `T` is — and the export is re-typed with a generic call signature. Row
 * checking is where it was always doing the work: `columns` and `rowKey` are
 * still verified against the element type of `rows`, and a `key` that is not a
 * property of the row is still a compile error.
 *
 * The cast is the price. Paying it once here is what keeps every call site from
 * paying it — the alternative, exporting the raw `ComponentFn`, makes each
 * table in an application either `any`-typed or wrapped by hand.
 */
export interface TableComponent {
  <T>(props: TableProps<T>): ElementNode<unknown>
  readonly def: ComponentDef<TableProps<AnyRow>>
  readonly displayName: string
}

export const Table = TableImpl as unknown as TableComponent
