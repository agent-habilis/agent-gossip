import { component } from 'visage-dom'
import { createElement } from 'visage-dom/element'
import type { Attrs } from 'visage-dom/types'
import { Style, css } from 'visage-style'
import type { BorderWeight } from 'moonspace'
import type { ColorRole } from 'moonspace-theme'
import { withStyle } from '../../internal.ts'
import { cells, rows } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export interface DividerProps extends Attrs<'div'> {
  /** Rule weight. Maps to the box-drawing characters `─`, `═`, `━`. */
  weight?: BorderWeight
  orientation?: 'horizontal' | 'vertical'
  color?: ColorRole
}

/**
 * `double` is a real CSS border style, so a 3px `double` renders as two
 * hairlines with a gap — the exact analogue of `═`. `thick` is a 2px solid rule,
 * matching `━`.
 */
const RULE = {
  line: '1px solid',
  double: '3px double',
  thick: '2px solid',
} as const satisfies Record<BorderWeight, string>

/**
 * One hoisted stylesheet for both orientations and all three weights, selected
 * by data attributes. Six combinations is small enough to enumerate; the colour
 * is not, so it arrives as `--ms-divider-color`.
 *
 * The `<hr>` reset the styled-components version carried — `border: 0` and
 * `margin: 0` — is gone with the `<hr>`. A `div` has neither by default.
 */
const DIVIDER = css({
  display: 'flex',
  flexShrink: 0,
  '&::before': { content: "''" },

  /*
   * A rule occupies one full row and centres its line within it, rather than
   * being a 1px element that knocks everything below it off the lattice. In a
   * terminal this is exactly one row of '─'.
   */
  '&[data-orientation="horizontal"]': {
    alignItems: 'center',
    width: '100%',
    height: rows(1),
    '&::before': {
      // A bare `1` here would compile to `flex:1px`, since `flex` is a shorthand
      // the grammar does not parse and a number means pixels everywhere else.
      flex: '1',
      borderTop: `${RULE.line} var(--ms-divider-color)`,
    },
    '&[data-weight="double"]::before': {
      borderTop: `${RULE.double} var(--ms-divider-color)`,
    },
    '&[data-weight="thick"]::before': {
      borderTop: `${RULE.thick} var(--ms-divider-color)`,
    },
  },

  '&[data-orientation="vertical"]': {
    justifyContent: 'center',
    width: cells(1),
    alignSelf: 'stretch',
    '&::before': {
      height: '100%',
      borderLeft: `${RULE.line} var(--ms-divider-color)`,
    },
    '&[data-weight="double"]::before': {
      borderLeft: `${RULE.double} var(--ms-divider-color)`,
    },
    '&[data-weight="thick"]::before': {
      borderLeft: `${RULE.thick} var(--ms-divider-color)`,
    },
  },
})

/**
 * A horizontal or vertical rule.
 *
 * Sized to consume exactly one cell across its thin axis, so it can be dropped
 * between rows or columns without shifting anything off the grid.
 *
 * Both orientations render a `<div role="separator">`, where the React version
 * used an `<hr>` for the horizontal case. `<hr>` is a void element and cannot
 * hold the scoped `<style>` child that carries the rules, so the semantics move
 * to the role — which is what an `<hr>` maps to in the accessibility tree
 * anyway.
 */
export const Divider = component<DividerProps>(function* (props) {
  yield () => {
    const { weight, orientation, color, style, attrs, ...rest } = props
    const axis = orientation ?? 'horizontal'

    return createElement(
      'div',
      {
        ...rest,
        dataset: { orientation: axis, weight: weight ?? 'line' },
        // Spread last so a caller's `aria-label` survives rather than replacing
        // the role this component exists to carry.
        attrs: { role: 'separator', 'aria-orientation': axis, ...attrs },
        style: withStyle(style, { '--ms-divider-color': String(t[color ?? 'border']) }),
      },
      [Style(DIVIDER)],
    )
  }
})
