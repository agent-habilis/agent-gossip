import { component } from 'visage-dom'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css } from 'visage-style'
import { glyphs } from 'moonspace'
import { INVERSE, ONE_ROW, SR_ONLY } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export interface RadioProps extends Omit<Attrs<'input'>, 'type'> {
  children?: Child
}

/**
 * One stylesheet for the whole control, anchored on the `<label>`.
 *
 * Same constraint as Checkbox, and worth restating because it is invisible when
 * you get it wrong: `&` is only substituted at the start of a key, so the
 * original's `input:focus-visible + &` would compile to literal nonsense and be
 * dropped silently. The selectors are written from the label and name the marker.
 * That is also why `Style()` is the first child — a `<style>` between the input
 * and the marker would break every `+` below.
 */
const RADIO = css({
  ...ONE_ROW,
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1ch',
  cursor: 'pointer',

  input: { ...SR_ONLY },

  /* Three cells: `(`, state, `)`. Parentheses distinguish radio from checkbox at a glance. */
  'input + span': { flex: 'none', width: '3ch', color: t.fg },

  'input:focus-visible + span': { ...INVERSE },
  'input:checked + span': { color: t.accent },
  'input:disabled + span': { color: t.fgSubtle },

  '&:has(input:disabled)': { color: t.fgSubtle, cursor: 'not-allowed' },
})

/**
 * A radio button drawn as `(•)`.
 *
 * Bracket shape carries the semantics: square brackets mean "any number of these",
 * parentheses mean "exactly one". That distinction is visible in monochrome, where
 * a color difference would not be.
 */
export const Radio = component<RadioProps>(function* (props) {
  yield () => {
    // Destructured here, not in the parameter list — see Checkbox. `class` and
    // `style` go to the label; everything else to the input.
    const { checked, children, class: className, style, ...rest } = props
    const state = checked ? glyphs.radio.on : glyphs.radio.off

    return (
      <label class={className} style={style}>
        {Style(RADIO)}
        <input type="radio" checked={checked} {...rest} />
        <span attrs={{ 'aria-hidden': 'true' }}>{`(${state})`}</span>
        {children != null && <span>{children}</span>}
      </label>
    )
  }
})
