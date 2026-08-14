import { component } from 'visage-dom'
import { createElement } from 'visage-dom/element'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { flag, withStyle } from '../../internal.ts'
import { cells, rows } from '../../mixins.ts'

type Align = 'start' | 'center' | 'end' | 'stretch' | 'baseline'
type Justify = 'start' | 'center' | 'end' | 'between' | 'around'

export interface StackProps extends Attrs<'div'> {
  /** Which element to render. Use this for semantics — it changes nothing visually. */
  as?: keyof HTMLElementTagNameMap
  /** Layout axis. */
  direction?: 'row' | 'column'
  /**
   * Space between children, measured in cells.
   *
   * The unit follows the axis: a `row` gaps by `n` columns, a `column` gaps by `n`
   * rows. One number, one meaning — "n cells of space" — regardless of direction.
   */
  gap?: number
  align?: Align
  justify?: Justify
  wrap?: boolean
  /**
   * Snap the container's own width down to a whole number of cells.
   *
   * Left unset, this turns itself on exactly when it matters — see `needsSnap`.
   * Pass it explicitly to force or suppress that.
   */
  snap?: boolean
  children?: Child
}

/**
 * Whether this Stack can place a child somewhere other than its own origin.
 *
 * If everything is packed from the start edge, the container's width is irrelevant:
 * children are laid out left-to-right from an origin that is already on the grid.
 * The moment alignment pushes a child toward the end or centre, that child's
 * position is measured from the container's *far* edge — so a container that is
 * 125.33 cells wide puts its last child a third of a character off the lattice.
 */
const needsSnap = (justify: Justify, align: Align): boolean =>
  justify !== 'start' || align === 'center' || align === 'end'

/**
 * One hoisted stylesheet for every combination, selected by data attributes.
 *
 * The two enumerable axes that used to be interpolated — `align` and `justify` —
 * are nine rules here rather than nine hundred generated classes, because the
 * keyword sets are closed. The gap is the one value that is not: it is any
 * number, so it travels as `--ms-stack-gap` and a single `gap` declaration reads
 * it.
 *
 * That property also absorbs the direction-dependent unit. `1ch` and
 * `calc(1 * var(--ms-row))` are the same "one cell" measured along different
 * axes, and resolving which one applies in the component keeps the CSS from
 * needing a rule per direction per gap.
 *
 * Order is load-bearing: the base declares the defaults (`column`, `stretch`,
 * `flex-start`, `nowrap`) and every block below is an equal-specificity
 * override, so the cascade breaks the tie on source order.
 */
const STACK = css({
  display: 'flex',
  flexDirection: 'column',
  alignItems: 'stretch',
  justifyContent: 'flex-start',
  flexWrap: 'nowrap',
  gap: raw('var(--ms-stack-gap)'),

  '&[data-direction="row"]': { flexDirection: 'row' },

  '&[data-align="start"]': { alignItems: 'flex-start' },
  '&[data-align="center"]': { alignItems: 'center' },
  '&[data-align="end"]': { alignItems: 'flex-end' },
  '&[data-align="baseline"]': { alignItems: 'baseline' },

  '&[data-justify="center"]': { justifyContent: 'center' },
  '&[data-justify="end"]': { justifyContent: 'flex-end' },
  '&[data-justify="between"]': { justifyContent: 'space-between' },
  '&[data-justify="around"]': { justifyContent: 'space-around' },

  '&[data-wrap="true"]': { flexWrap: 'wrap' },

  /*
   * Deliberately not the `SNAP` mixin, which also sets `width: 100%`. A Stack
   * has no opinion about its own width until snapping is asked for; forcing
   * `100%` would make every aligned row fill its parent, which is a layout
   * change and not the one this prop is for.
   */
  '&[data-snap="true"]': {
    '@supports (width: round(down, 100%, 1ch))': {
      width: raw('round(down, 100%, 1ch)'),
    },
  },
})

/**
 * One-dimensional layout on the character grid.
 *
 * Gaps are always whole cells, which is the entire point: a `Stack` can only produce
 * spacings that a terminal could reproduce by emitting spaces or newlines. There is
 * no `gap={1.5}` because there is no half a character.
 */
export const Stack = component<StackProps>(function* (props) {
  yield () => {
    // Destructured inside the thunk, never in the parameter list: `props` is a
    // live tracking proxy, and reading it here is what subscribes the component.
    const { as, direction, gap, align, justify, wrap, snap, children, style, ...rest } = props

    const axis = direction ?? 'column'
    const alignment = align ?? 'stretch'
    const distribution = justify ?? 'start'
    const cellCount = gap ?? 0

    /*
     * `createElement` rather than JSX, because the tag is a value. A lowercase
     * identifier in JSX is read as a literal tag name, and a capitalised one
     * makes TypeScript intersect every element's attribute type — 174 of them.
     */
    return createElement(
      as ?? 'div',
      {
        ...rest,
        dataset: {
          direction: axis,
          align: alignment,
          justify: distribution,
          wrap: flag(wrap),
          snap: flag(snap ?? needsSnap(distribution, alignment)),
        },
        style: withStyle(style, {
          '--ms-stack-gap': axis === 'row' ? cells(cellCount) : rows(cellCount),
        }),
      },
      [Style(STACK), children],
    )
  }
})
