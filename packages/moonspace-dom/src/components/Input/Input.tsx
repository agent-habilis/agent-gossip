import { component } from 'visage-dom'
import { createElement } from 'visage-dom/element'
import type { Attrs } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { flag, withStyle } from '../../internal.ts'
import { ONE_ROW, SNAP, cells } from '../../mixins.ts'
import { t } from '../../tokens.ts'

/*
 * `width` is omitted from the native attributes before being redeclared. The
 * DOM's `input.width` is the pixel width of an `<input type="image">`; ours is a
 * count of cells, and they must not be the same prop.
 */
export interface InputProps extends Omit<Attrs<'input'>, 'width'> {
  /** Width in cells. Omit to fill the parent, snapped to whole cells. */
  width?: number
  /**
   * Marks the field invalid. Renders a `!` marker as well as recolouring, so the
   * state is readable without color.
   */
  invalid?: boolean
  /** A single-character marker shown before the value, e.g. `>` or `/`. */
  prefix?: string
}

/**
 * One hoisted stylesheet for the wrapper, the marker and the field.
 *
 * The three styled-components collapse into one object because `@scope` binds
 * the rules to the wrapper: `input` here already means `:scope input`, so the
 * inner elements need no name of their own.
 *
 * The wrapper is also the only element that *can* carry the `<style>`. An
 * `<input>` is a void element and cannot host a child node, so a component
 * whose root was the field itself would have nowhere to put its own scope.
 *
 * Order is load-bearing: base, then the width and invalid variants, then the
 * focus and disabled states, which share their specificity with the variants.
 */
const INPUT = css({
  ...ONE_ROW,
  display: 'flex',
  alignItems: 'center',
  gap: 0,
  paddingInline: cells(1),
  background: t.bgSunken,

  /* Outline, not border — see Box for why this matters to the grid. */
  outline: `${t.msBorderWidth} solid ${t.border}`,
  outlineOffset: 0,

  /*
   * A cell count cannot be enumerated, so the fixed case reads a custom property
   * the component sets per instance; the fill case is the shared `SNAP`. The two
   * are mutually exclusive, which is why the fixed width is not on the base rule
   * — an unset `var()` is invalid at computed-value time and would resolve to
   * `auto` rather than falling through.
   */
  '&[data-width="fixed"]': { width: raw('var(--ms-input-width)') },
  '&[data-width="fill"]': { ...SNAP },

  '&[data-invalid="true"]': { outlineColor: t.danger },

  '&:focus-within': { outlineColor: t.accent },
  // Invalid outranks focus, and only a compound selector can say so: the two
  // rules above are equal on specificity.
  '&[data-invalid="true"]:focus-within': { outlineColor: t.danger },

  /*
   * The marker column. `aria-hidden`, so it is the sighted half of a state the
   * `aria-invalid` attribute carries for a screen reader.
   */
  span: { flex: 'none', width: cells(2), color: t.fgSubtle },
  '&[data-invalid="true"] span': { color: t.danger },

  input: {
    ...ONE_ROW,
    // `'1'` as a string: `flex` is kind `raw`, and a bare number would be
    // serialised as a length — `flex: 1px`.
    flex: '1',
    minWidth: 0,
    padding: 0,
    border: 0,
    outline: 0,
    background: 'transparent',
    color: t.fg,
  },
  'input::placeholder': { color: t.fgSubtle },
  'input:disabled': { color: t.fgSubtle, cursor: 'not-allowed' },

  /* Native affordances are all off-grid graphics. Remove them. */
  'input[type="search"]::-webkit-search-decoration': { appearance: 'none' },
  'input[type="search"]::-webkit-search-cancel-button': { appearance: 'none' },
  'input::-webkit-outer-spin-button': { appearance: 'none', margin: 0 },
  'input::-webkit-inner-spin-button': { appearance: 'none', margin: 0 },

  // Last, and by specificity as well as order: `:has()` takes the specificity of
  // its argument, so this beats every variant above it.
  '&:has(input:disabled)': { background: 'transparent', outlineColor: t.border },
})

/**
 * A single-row text field.
 *
 * The invalid state shows a `!` marker in the prefix column rather than relying on
 * the red outline alone — the outline is a color signal and disappears in a
 * monochrome terminal, while the marker does not.
 *
 * A real `<input>` does the work: keyboard behaviour, form participation and the
 * accessibility tree are the platform's, and only the appearance is replaced.
 */
export const Input = component<InputProps>(function* (props) {
  yield () => {
    /*
     * Destructured here rather than in the parameter list. `props` is a live
     * tracking proxy: reading it inside the thunk is what subscribes this
     * component to the keys it uses, and destructuring in the signature throws.
     *
     * `class` and `style` are peeled off for the wrapper — a caller styling an
     * Input means the control, which is the box — while everything else is a
     * native input attribute and belongs on the field, `ref` included.
     */
    const { width, invalid, prefix, class: cls, style, attrs, ...rest } = props
    const marker = invalid === true ? '!' : prefix

    return createElement(
      'div',
      {
        class: cls,
        style: withStyle(
          style,
          width === undefined ? {} : { '--ms-input-width': cells(width) },
        ),
        dataset: {
          width: width === undefined ? 'fill' : 'fixed',
          invalid: flag(invalid),
        },
      },
      [
        Style(INPUT),
        marker === undefined
          ? null
          : createElement('span', { attrs: { 'aria-hidden': 'true' } }, [marker]),
        createElement(
          'input',
          {
            // Caller attrs last, as the spread order was in the React version:
            // an explicit `aria-invalid` from the outside still wins.
            attrs: { 'aria-invalid': invalid === true ? 'true' : undefined, ...attrs },
            ...rest,
          },
          [],
        ),
      ],
    )
  }
})
