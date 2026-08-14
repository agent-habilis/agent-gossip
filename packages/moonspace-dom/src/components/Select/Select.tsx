import { component } from 'visage-dom'
import type { Attrs } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { glyphs } from 'moonspace'
import { ONE_ROW, SNAP, cells } from '../../mixins.ts'
import { flag, withStyle } from '../../internal.ts'
import { t } from '../../tokens.ts'

export interface SelectOption {
  value: string
  label: string
  disabled?: boolean
}

export interface SelectProps extends Attrs<'select'> {
  options: readonly SelectOption[]
  /** Width in cells. Omit to fill the parent, snapped to whole cells. */
  width?: number
  placeholder?: string
}

/**
 * One stylesheet, anchored on the wrapper.
 *
 * The wrapper is not a stylistic choice here: `<select>` is not a valid parent
 * for a `<style>` element, so the scope root has to be something else and the
 * control is reached with a descendant selector. Three styled elements collapse
 * into one sheet as a result.
 *
 * Width is the one per-instance value, and it arrives as a custom property
 * because a hoisted sheet cannot hold it. `data-fixed` picks which of the two
 * rules wins; the attribute out-specifies the bare `:scope` that `SNAP` writes,
 * inside the `@supports` guard as well as outside it.
 */
const SELECT = css({
  ...ONE_ROW,
  ...SNAP,
  position: 'relative',
  display: 'flex',
  alignItems: 'center',
  paddingInline: '1ch',
  background: t.bgSunken,
  outline: `var(--ms-border-width) solid ${t.border}`,
  outlineOffset: 0,

  '&[data-fixed="true"]': { width: raw('var(--ms-select-width)') },

  /*
   * The ring is on the wrapper, not the control, because the control is what has
   * focus and the border being lit is what the user reads as the field.
   */
  '&:focus-within': { outlineColor: t.accent },

  '&:has(select:disabled)': { background: 'transparent', color: t.fgSubtle },

  select: {
    ...ONE_ROW,
    // `flex: 1` would serialise as `1px` — the property's kind is `raw`, so a
    // bare number gets a unit. The string is the only safe spelling.
    flex: '1',
    minWidth: 0,
    padding: 0,
    border: 0,
    outline: 0,
    background: 'transparent',
    color: 'inherit',
    cursor: 'pointer',

    /*
     * Strip the platform chevron. It is a vector glyph at an arbitrary size and
     * position, and it would be the only thing on screen not sitting in a cell.
     */
    appearance: 'none',
  },

  'select:disabled': { cursor: 'not-allowed' },

  /*
   * The dropdown list itself is drawn by the OS and cannot be styled, so it will
   * not be on the grid. That is a deliberate trade: native gives us keyboard
   * handling, type-ahead, mobile pickers and screen-reader support for free, and
   * the popup is transient. A fully custom listbox would be grid-perfect and would
   * also mean reimplementing all of that.
   */
  option: { background: t.bgSunken, color: t.fg },

  /* Our own chevron, occupying exactly one cell in the last column. */
  span: {
    flex: 'none',
    width: '2ch',
    textAlign: 'right',
    color: t.fgMuted,
    pointerEvents: 'none',
  },
})

/**
 * A single-row select, rendered as `value            ▾`.
 *
 * Wraps a native `<select>` so keyboard and assistive-technology behaviour is the
 * platform's, while the closed state stays on the character grid.
 */
export const Select = component<SelectProps>(function* (props) {
  yield () => {
    // Destructured inside the thunk, not in the parameter list: `props` is a live
    // tracking proxy. `class` and `style` describe the field as a whole, so they
    // go to the wrapper; every real select attribute passes through to the control.
    const { options, width, placeholder, class: className, style, ...rest } = props

    /*
     * The selection is carried by the `<option>`, not by the `<select>`.
     *
     * `value` on a select is a property assignment, and the reconciler applies
     * an element's props before mounting its children — so at the moment
     * `el.value = 'preview'` runs there are no options to match, the assignment
     * is discarded, and the control silently shows the first option instead.
     * Marking the matching option `selected` states the same thing in a way that
     * does not depend on ordering, which is also how HTML expresses it.
     */
    const value = rest.value

    return (
      <div
        class={className}
        dataset={{ fixed: flag(width !== undefined) }}
        style={withStyle(style, width === undefined ? {} : { '--ms-select-width': cells(width) })}
      >
        {Style(SELECT)}
        <select {...rest}>
          {placeholder != null && (
            <option value="" disabled selected={value === ''}>
              {placeholder}
            </option>
          )}
          {options.map((option) => (
            <option
              key={option.value}
              value={option.value}
              disabled={option.disabled}
              selected={value === option.value}
            >
              {option.label}
            </option>
          ))}
        </select>
        <span attrs={{ 'aria-hidden': 'true' }}>{glyphs.chevron.down}</span>
      </div>
    )
  }
})
