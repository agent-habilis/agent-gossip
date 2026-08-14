import { component } from 'visage-dom'
import { createElement } from 'visage-dom/element'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { fontWeight as WEIGHT } from 'moonspace'
import type { FontWeight } from 'moonspace'
import type { ColorRole } from 'moonspace-theme'
import { flag, withStyle } from '../../internal.ts'
import { t } from '../../tokens.ts'

export interface TextProps extends Attrs<'span'> {
  /** Which element to render. Use this for semantics — it changes nothing visually. */
  as?: keyof HTMLElementTagNameMap
  /** Semantic color role. */
  color?: ColorRole
  /** Regular or bold. There is nothing between them. */
  weight?: FontWeight
  /** Swap foreground and background. The system's strongest emphasis. */
  inverse?: boolean
  /** Uppercase. Used for section labels, where a size change would normally be used. */
  caps?: boolean
  /** Clip to one line with a trailing ellipsis. For paths and IDs use MiddleTruncate. */
  truncate?: boolean
  children?: Child
}

/**
 * One hoisted stylesheet for every combination, selected by data attributes.
 *
 * The colour is the exception, and it is why `--ms-text-color` exists. Sixteen
 * semantic roles crossed with `inverse` would be thirty-two blocks in this
 * object; a single custom property set on the element collapses that to two
 * rules that both read it. The role name never reaches the CSS — the component
 * resolves it to `var(--accent)` and assigns that.
 *
 * Order is load-bearing. `:scope[data-inverse="true"]` and
 * `:scope[data-caps="true"]` have equal specificity, so the cascade breaks ties
 * on source order: base first, then the modifiers.
 */
const TEXT = css({
  margin: 0,
  /*
   * `raw()` around a number that is already valid CSS, which looks redundant and
   * is not.
   *
   * `fontWeight`'s grammar admits the numeric literals, but the serialiser
   * appends `px` to any bare number whose property is not in its unitless set,
   * and `font-weight` is not in that set — so `fontWeight: 400` typechecks and
   * emits `font-weight:400px`, which every browser drops. The prop would render,
   * pass tsc, and do nothing. `raw()` is the only spelling that both typechecks
   * and survives serialisation while keeping the value tied to the theme.
   */
  fontWeight: raw(`${WEIGHT.regular}`),
  color: raw('var(--ms-text-color)'),

  '&[data-weight="bold"]': { fontWeight: raw(`${WEIGHT.bold}`) },

  /*
   * Monospace means uppercase costs no extra width — the advance is identical.
   * That is exactly why caps can carry hierarchy here when a type scale cannot.
   */
  '&[data-caps="true"]': { textTransform: 'uppercase' },

  '&[data-inverse="true"]': {
    background: raw('var(--ms-text-color)'),
    color: t.bg,
    /*
     * Pad by one cell so the inverted block reads as a filled region rather than
     * text with a tight highlight, and stay on the grid by using whole cells.
     * The negative margin cancels the pad so surrounding text does not shift.
     */
    paddingInline: '1ch',
    marginInline: '-1ch',
  },

  '&[data-truncate="true"]': {
    display: 'block',
    overflow: 'hidden',
    whiteSpace: 'nowrap',
    textOverflow: 'ellipsis',
  },
})

/**
 * The only typographic component.
 *
 * There is no `size` prop, and there will not be one — this system has a single
 * font size by design. Hierarchy is built from four things instead: color,
 * weight, capitalisation, and inversion. Every one of them survives a terminal;
 * a type scale would not.
 */
export const Text = component<TextProps>(function* (props) {
  yield () => {
    /*
     * Destructured here rather than in the parameter list. `props` is a live
     * tracking proxy: reading it inside the thunk is what subscribes this
     * component to the keys it uses, and destructuring in the signature throws.
     */
    const { as, color, weight, inverse, caps, truncate, children, style, ...rest } = props

    return createElement(
      as ?? 'span',
      {
        ...rest,
        dataset: {
          weight: weight ?? 'regular',
          inverse: flag(inverse),
          caps: flag(caps),
          truncate: flag(truncate),
        },
        /*
         * `t` is keyed by role, so `t[color]` is already the `var(--accent)`
         * string the property needs — no name mangling, and a role that does not
         * exist is a compile error rather than an empty custom property.
         */
        style: withStyle(style, { '--ms-text-color': String(t[color ?? 'fg']) }),
      },
      [Style(TEXT), children],
    )
  }
})
