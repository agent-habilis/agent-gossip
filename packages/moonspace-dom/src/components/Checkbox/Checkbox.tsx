import { component } from 'visage-dom'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css } from 'visage-style'
import { glyphs } from 'moonspace'
import { INVERSE, ONE_ROW, SR_ONLY } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export interface CheckboxProps extends Omit<Attrs<'input'>, 'type'> {
  /** Renders the mixed state, `[-]`. Does not affect `checked` either way. */
  indeterminate?: boolean
  children?: Child
}

/**
 * One stylesheet for the whole control, anchored on the `<label>`.
 *
 * The styled-components original wrote the state rules on the marker itself, as
 * `input:focus-visible + &`. That shape cannot survive here: the compiler
 * substitutes `&` only when it is the first character of a key, so anywhere else
 * it is emitted verbatim and the browser drops the rule without a word. Naming
 * the marker element and anchoring the selector at the label says the same thing
 * — and collapses what were three styled elements into one sheet.
 *
 * Which makes the child order load-bearing rather than stylistic. `Style()` is an
 * ordinary element occupying an ordinary sibling slot, so a `<style>` between the
 * input and the marker would stop `+` matching. It goes first.
 */
const CHECKBOX = css({
  ...ONE_ROW,
  display: 'inline-flex',
  alignItems: 'center',
  gap: '1ch',
  cursor: 'pointer',

  input: { ...SR_ONLY },

  /*
   * Three cells wide, always: `[`, the state character, `]`. Fixing the width
   * means a column of checkboxes has its labels aligned without any extra layout.
   */
  'input + span': { flex: 'none', width: '3ch', color: t.fg },

  /* Focus inverts the marker — legible without color, unlike a ring. */
  'input:focus-visible + span': { ...INVERSE },

  'input:checked + span': { color: t.accent },

  /*
   * Mixed reads as a weaker "on", not as "off" — some of what this box stands
   * for is checked. Two keys rather than one comma-separated selector: only the
   * first half of a list gets the `:scope` prefix, so `a, b` would compile to
   * `:scope a, b` and leak the second half onto the whole document.
   */
  'input:indeterminate + span': { color: t.accent },

  'input:disabled + span': { color: t.fgSubtle },

  '&:has(input:disabled)': { color: t.fgSubtle, cursor: 'not-allowed' },
})

/**
 * A checkbox drawn as `[x]`.
 *
 * The real `<input>` is present but visually hidden, so keyboard behaviour, form
 * participation and screen-reader semantics are all native; only the appearance is
 * replaced. That pattern repeats across every control in this system.
 */
export const Checkbox = component<CheckboxProps>(function* (props) {
  yield () => {
    /*
     * Destructured inside the thunk, never in the parameter list: `props` is a
     * live tracking proxy, and reading it here is what subscribes the component
     * to the keys it uses.
     *
     * `class` and `style` are pulled out of the rest so they land on the label
     * rather than on the input, which is the element nobody can see.
     */
    const { indeterminate, checked, children, class: className, style, ...rest } = props

    const state = indeterminate
      ? glyphs.checkbox.mixed
      : checked
        ? glyphs.checkbox.on
        : glyphs.checkbox.off

    return (
      <label class={className} style={style}>
        {Style(CHECKBOX)}
        {/*
         * `indeterminate` is a DOM property with no matching attribute, so React
         * could only fake it with `aria-checked`. Here it can be set for real,
         * and the platform's own mixed state carries the semantics.
         */}
        <input type="checkbox" checked={checked} indeterminate={indeterminate} {...rest} />
        <span attrs={{ 'aria-hidden': 'true' }}>{`[${state}]`}</span>
        {children != null && <span>{children}</span>}
      </label>
    )
  }
})
