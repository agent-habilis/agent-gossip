import { component } from 'visage-dom'
import { createElement } from 'visage-dom/element'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import type { BorderWeight } from 'moonspace'
import type { ColorRole } from 'moonspace-theme'
import { flag, withStyle } from '../../internal.ts'
import { SNAP, SNAP_MEASURE, cells, rows } from '../../mixins.ts'
import { t } from '../../tokens.ts'
import { Divider } from '../Divider/index.ts'
import { Text } from '../Text/index.ts'

export type BoxBorder = BorderWeight | 'none'

export interface BoxProps extends Omit<Attrs<'div'>, 'title'> {
  /** Which element to render. Use this for semantics — it changes nothing visually. */
  as?: keyof HTMLElementTagNameMap
  /** Border weight, or `none`. Maps to `│`, `║`, `┃`. */
  border?: BoxBorder
  borderColor?: ColorRole
  background?: ColorRole
  /** Horizontal padding in cells. */
  padX?: number
  /** Vertical padding in rows. */
  padY?: number
  /**
   * Width in cells, or a keyword:
   * - `full` — fill the parent, snapped down to a whole cell
   * - `measure` — the default 80-column measure, or the parent if narrower
   * - `auto` — shrink to content
   */
  width?: number | 'full' | 'measure' | 'auto'
  /** Optional header row, rendered inside the box above a rule. */
  title?: Child
  /**
   * Escape hatch. Skips cell-snapping for content that genuinely fights the grid
   * — embedded media, third-party widgets. Use sparingly; it is the one place the
   * system stops guaranteeing terminal parity.
   */
  unsnapped?: boolean
  children?: Child
}

/** The border weights, as outlines. `none` is the absence of a block, not a value. */
const OUTLINE = {
  line: '1px solid',
  double: '3px double',
  thick: '2px solid',
} as const satisfies Record<BorderWeight, string>

/**
 * One hoisted stylesheet, selected by `data-border`, `data-width` and
 * `data-unsnapped`.
 *
 * Everything left over is per-instance and unbounded — a padding in cells, a
 * width in cells, two colour roles — so it travels as custom properties on the
 * inline style. Building a style object per Box instead would miss the compile
 * cache and produce one stylesheet per padding value.
 *
 * Order is load-bearing throughout: `:scope[data-width="full"]` and
 * `:scope[data-unsnapped="true"]` have equal specificity, so the unsnapped
 * override only wins by being written after the block it undoes.
 */
const BOX = css({
  // Two axes rather than one `padding`, because the shorthand takes up to four
  // lengths and the grammar's `Length` is exactly one.
  paddingBlock: raw('var(--ms-box-pad-y)'),
  paddingInline: raw('var(--ms-box-pad-x)'),
  background: 'var(--ms-box-bg)',
  // Flush against the cell boundary rather than bleeding into the next cell.
  outlineOffset: 0,

  /*
   * Borders are drawn with outline, not border, and this is the single most
   * load-bearing decision in the component.
   *
   * A CSS border participates in layout: a 1px border on each side makes the box
   * 2px taller than a whole number of rows. Nest or stack a few of those and every
   * element below has drifted off the lattice by an amount that is invisible in
   * isolation and glaring in aggregate.
   *
   * An outline has zero layout impact. The box still measures a whole number of
   * cells; the rule is painted on the cell boundary — which is precisely where a
   * terminal draws it, since a border character *is* the boundary between two
   * regions rather than something occupying space inside one.
   */
  '&[data-border="line"]': { outline: `${OUTLINE.line} var(--ms-box-border-color)` },
  '&[data-border="double"]': { outline: `${OUTLINE.double} var(--ms-box-border-color)` },
  '&[data-border="thick"]': { outline: `${OUTLINE.thick} var(--ms-box-border-color)` },

  '&[data-width="auto"]': { width: 'max-content' },
  '&[data-width="fixed"]': { width: raw('var(--ms-box-width)') },
  '&[data-width="full"]': { ...SNAP },
  '&[data-width="measure"]': { ...SNAP_MEASURE },

  /*
   * The escape hatch, expressed as an override rather than as a fourth width
   * value: it drops the `round()` that `SNAP` and `SNAP_MEASURE` install and
   * leaves everything else — including the measure ceiling, which is restored
   * below because `SNAP_MEASURE` folds `round()` into its `max-width` too.
   */
  '&[data-unsnapped="true"]': { width: '100%' },
  '&[data-width="measure"][data-unsnapped="true"]': { maxWidth: t.msMeasure },
})

/**
 * A bordered, padded region — the system's panel and window primitive.
 *
 * The optional `title` renders as a real header row inside the border rather than
 * as a legend notched into the top edge. The notched form is more idiomatic to
 * terminal UIs, but in a browser it forces the label onto a half-row offset; a
 * header row is honest about occupying a row, which is what a terminal does anyway.
 */
export const Box = component<BoxProps>(function* (props) {
  yield () => {
    const {
      as,
      border,
      borderColor,
      background,
      padX,
      padY,
      width,
      title,
      unsnapped,
      children,
      style,
      ...rest
    } = props

    const rule = borderColor ?? 'border'
    const size = width ?? 'full'

    return createElement(
      /*
       * `createElement` rather than JSX, because JSX cannot express a tag that
       * is only known at runtime: a lowercase identifier is read as a literal
       * tag name, and a capitalised one makes TypeScript intersect the attribute
       * types of all 174 elements.
       */
      as ?? 'div',
      {
        ...rest,
        dataset: {
          border: border ?? 'none',
          // A numeric width is one case, not one value — the number itself lives
          // in `--ms-box-width`, so the selector only has to know the kind.
          width: typeof size === 'number' ? 'fixed' : size,
          unsnapped: flag(unsnapped),
        },
        style: withStyle(style, {
          '--ms-box-pad-x': cells(padX ?? 1),
          '--ms-box-pad-y': rows(padY ?? 0),
          '--ms-box-bg': background === undefined ? 'transparent' : String(t[background]),
          '--ms-box-border-color': String(t[rule]),
          ...(typeof size === 'number' ? { '--ms-box-width': cells(size) } : null),
        }),
      },
      [
        Style(BOX),
        title == null ? null : [
          <Text as="div" color="fgMuted" caps>{title}</Text>,
          <Divider color={rule} />,
        ],
        children,
      ],
    )
  }
})
