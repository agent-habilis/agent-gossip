import { component } from 'visage-dom'
import { createElement } from 'visage-dom/element'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { flag } from '../../internal.ts'
import { ONE_ROW, SNAP } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'danger'

export interface ButtonProps extends Attrs<'button'> {
  variant?: ButtonVariant
  /** Fill the available width, snapped to whole cells. */
  block?: boolean
  children?: Child
}

/**
 * One hoisted stylesheet for every variant, selected by data attributes.
 *
 * VENDOR FORK: upstream draws buttons as literal `[ Label ]` brackets. This
 * copy keeps the boxed presentation the app shipped with before the re-vendor —
 * a one-row chrome (padding, inset outline, per-variant fill) and lowercase
 * labels. Colours come from the current tokens, so the fork is shape-only.
 *
 * The affordance is the box: a fill for the three variants that carry weight,
 * nothing at all for `ghost`. Bracket pseudo-elements meant `primary` painted
 * its accent edge-to-edge across them with no breathing room and read as a
 * highlighted string rather than a control.
 *
 * Order is load-bearing. `:scope[data-variant="primary"]` and
 * `:scope:focus-visible` are both specificity (0,2,0), so the cascade breaks
 * ties on source order: base, then variants, then states.
 */
const BUTTON = css({
  /*
   * The one-row chrome (was `oneRowChrome` in the old moonspace-ui mixins).
   *
   * `1ch` is one cell, so it is the horizontal padding; the row's own leading
   * is the vertical padding. The edge is an `outline` — it costs no layout, so
   * the content box stays a full row — declared transparent up front so states
   * only ever set `outlineColor`. The offset is negative so the ring paints
   * just *inside* the border box: an outline drawn outside is at the mercy of
   * any ancestor that clips, and the download button sits flush against a
   * scroller that clips horizontally. `border: 0` is not redundant: a bare
   * `<button>` gets `2px outset` from the UA stylesheet, which pushes the
   * control 4px past a whole number of cells.
   */
  ...ONE_ROW,
  display: 'inline-flex',
  alignItems: 'center',
  padding: raw('0 1ch'),
  border: 0,
  outline: raw('var(--ms-border-width) solid transparent'),
  outlineOffset: raw('-1px'),
  background: 'transparent',
  cursor: 'pointer',
  whiteSpace: 'nowrap',
  /*
   * Casing is the component's, not the caller's. `text-transform` is
   * presentation, so the accessible name keeps its capitalisation.
   */
  textTransform: 'lowercase',
  // `secondary` is the default variant, so its colour is the base colour.
  color: t.fg,

  '&[data-variant="primary"]': { background: t.accent, color: t.fgOnAccent },
  '&[data-variant="secondary"]': { background: t.bgRaised },
  /*
   * Ghost carries the base text colour and no fill. Muted text was an earlier
   * distinction, and it made the button read as de-emphasised prose rather
   * than as a control of equal rank sitting one weight down — the missing box
   * already says that, without touching legibility.
   */
  '&[data-variant="danger"]': { background: t.danger, color: t.fgOnAccent },

  '&[data-block="true"]': { display: 'flex', justifyContent: 'center', ...SNAP },

  /*
   * Hover is one step further up the same stack the button already sits on:
   * sunken page, raised button, this. Nothing is outlined, so the fill is the
   * only thing that can carry the state.
   */
  '&[data-variant="secondary"]:hover:not(:disabled)': { background: t.borderStrong },
  /*
   * Ghost steps up to exactly what `secondary` paints at rest, rather than to
   * `secondary`'s own hover — hovering onto the same fill would collapse the
   * two and leave the lighter control looking heavier than the one above it.
   */
  '&[data-variant="ghost"]:hover:not(:disabled)': { background: t.bgRaised },

  /*
   * Focus recolours the edge rather than drawing a second ring — there is only
   * one outline to go around, and it is already flush with the cell boundary.
   */
  '&:focus-visible': { outlineColor: t.accent },
  /*
   * A ring has to contrast with what it surrounds, and `primary` is filled
   * with the ring's own colour — accent on accent is invisible. `danger` needs
   * no exception; an accent ring on the danger fill is already its own signal.
   */
  '&[data-variant="primary"]:focus-visible': { outlineColor: t.fg },

  /*
   * Not the shared `DISABLED` fragment: that sets `pointer-events: none`, which
   * would also suppress the `not-allowed` cursor it asks for. A button already
   * refuses its own events when disabled, so only the appearance is stated here.
   */
  '&:disabled': {
    color: t.fgSubtle,
    cursor: 'not-allowed',
  },
  /*
   * Ghost is the one variant with no box to grey out, so it keeps none here
   * either — the subtle text is the whole signal. Painting the shared fill
   * would make the box *appear* on the way to disabled and vanish on the way
   * back, which is the one thing the variant exists not to do.
   */
  '&:disabled:not([data-variant="ghost"])': {
    background: t.bgRaised,
  },
})

/**
 * A single-row action control, rendered as a boxed fill (vendor fork; upstream
 * renders `[ Label ]`).
 *
 * Height is always exactly one row, so a button can sit inline in a row of
 * text without disturbing the grid.
 */
export const Button = component<ButtonProps>(function* (props) {
  yield () => {
    /*
     * Destructured here rather than in the parameter list. `props` is a live
     * tracking proxy: reading it inside the thunk is what subscribes this
     * component to the keys it uses, and destructuring in the signature throws.
     */
    const { variant, block, type, children, ...rest } = props

    return createElement(
      'button',
      {
        ...rest,
        // A `<button>` inside a form submits it unless told otherwise, which is
        // never what an unqualified `<Button>` means.
        type: type ?? 'button',
        dataset: { variant: variant ?? 'secondary', block: flag(block) },
      },
      [Style(BUTTON), children],
    )
  }
})
