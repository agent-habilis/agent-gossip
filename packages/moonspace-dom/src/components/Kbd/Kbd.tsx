import { component } from 'visage-dom'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css } from 'visage-style'
import { ONE_ROW } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export interface KbdProps extends Attrs<'kbd'> {
  children?: Child
}

/**
 * No variants, so no data attributes and nothing per instance — the whole
 * component is this one block.
 *
 * `paddingInline` rather than the `padding: 0 1ch` shorthand: the value grammar
 * types `padding` as a single length, so a two-value shorthand would have to go
 * through `raw()`. The logical property says the same thing and stays checked.
 */
const KBD = css({
  ...ONE_ROW,
  display: 'inline-flex',
  alignItems: 'center',
  paddingInline: '1ch',
  /*
   * Reverse video rather than the usual raised-keycap treatment. A keycap is built
   * from a border radius and a drop shadow, neither of which exists in a terminal;
   * inversion is how a TUI has always marked a key hint, and it costs exactly the
   * label's width plus two cells.
   */
  background: t.bgRaised,
  color: t.fg,
  whiteSpace: 'nowrap',
})

/**
 * A keyboard key hint.
 *
 * Sized to its label plus two cells, so a sequence of hints stays on the grid:
 * `⌘K` is four cells, `Esc` is five.
 */
export const Kbd = component<KbdProps>(function* (props) {
  yield () => {
    // Inside the thunk, not the parameter list — destructuring `props` in the
    // signature throws, and reading it here is what tracks the keys used.
    const { children, ...rest } = props

    return (
      <kbd {...rest}>
        {/* First child, so the <style> takes the `:first-child` slot. */}
        {Style(KBD)}
        {children}
      </kbd>
    )
  }
})
