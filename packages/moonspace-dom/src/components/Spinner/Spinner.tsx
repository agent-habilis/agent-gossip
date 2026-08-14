import { component, disposable, interval, signal } from 'visage-dom'
import { createElement } from 'visage-dom/element'
import type { Attrs } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { asciiSpinnerFrames, spinnerFrames } from 'moonspace'
import type { ColorRole } from 'moonspace-theme'
import { withStyle } from '../../internal.ts'
import { ONE_ROW, SR_ONLY, cells } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export interface SpinnerProps extends Attrs<'span'> {
  /** Milliseconds per frame. Fixed for the instance's lifetime — see below. */
  interval?: number
  /** Use the `|/-\` frames instead of Braille, for terminals with patchy coverage. */
  ascii?: boolean
  color?: ColorRole
  /** Accessible status text, e.g. "Building". */
  label?: string
}

/**
 * The two parts are addressed by `data-part` rather than by position, so the
 * frame keeps its rule if a third child is ever added between them.
 *
 * The colour is a custom property for the same reason it is one in `Text`:
 * sixteen roles would otherwise be sixteen blocks, and the role name never
 * needs to reach the CSS.
 */
const SPINNER = css({
  ...ONE_ROW,
  display: 'inline-flex',
  alignItems: 'center',
  gap: cells(1),

  '& > [data-part="frame"]': {
    flex: 'none',
    /*
     * Pinned to one cell. Braille glyphs are single-width, but reserving the
     * cell explicitly means neighbouring content cannot shift by a fraction of
     * a character as the frames cycle.
     */
    width: cells(1),
    color: raw('var(--ms-spinner-color)'),
  },

  '& > [data-part="label"]': { ...SR_ONLY },
})

/**
 * An indeterminate activity indicator, animated by cycling Braille characters.
 *
 * Animated in JavaScript rather than with a CSS keyframe because the animation
 * *is* a sequence of characters — the same ten frames a terminal spinner emits.
 * A CSS rotation would look smoother in a browser and would have no terminal
 * equivalent; `moonspace-tui`'s Spinner is that equivalent, and it is the same
 * component down to the `using`.
 *
 * The one stateful component on this side of the system, along with
 * `MiddleTruncate`. Everything else is a pure function of its props.
 *
 * Honours `prefers-reduced-motion` by holding a single frame.
 */
export const Spinner = component<SpinnerProps>(function* (props) {
  /*
   * Setup runs once per instance, which is what fixes the tick for the
   * instance's lifetime. That is a deliberate narrowing of the React version,
   * whose effect re-subscribed whenever `interval` changed: a `using` cannot be
   * re-acquired without a remount, and nothing passes a changing interval to a
   * spinner. The reduced-motion query is read once for the same reason.
   */
  const index = signal(0)
  const every = props.interval ?? 80
  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches

  /*
   * A ternary rather than an `if`, and that is not style. A `using` declared
   * inside an `if` block is disposed at the end of *that block* — the timer
   * would be cleared microseconds after it started rather than at unmount.
   */
  using _tick = reduced
    ? disposable(() => {})
    : interval(every, () => {
        index.value += 1
      })

  /*
   * Never wrapped, so the count is independent of how many frames the current
   * set has — `ascii` can flip between two sets of different lengths without
   * the animation jumping.
   */
  yield () => {
    const { ascii, color, label, interval: _interval, style, ...rest } = props
    const frames = ascii === true ? asciiSpinnerFrames : spinnerFrames
    const frame = frames[index.value % frames.length] as string

    return createElement(
      'span',
      {
        ...rest,
        attrs: { role: 'status' },
        style: withStyle(style, { '--ms-spinner-color': String(t[color ?? 'accent']) }),
      },
      [
        Style(SPINNER),
        /*
         * The glyph is hidden from assistive technology and the label is hidden
         * from sight, so the status is announced as words rather than as a
         * Braille character read aloud.
         */
        createElement(
          'span',
          { dataset: { part: 'frame' }, attrs: { 'aria-hidden': 'true' } },
          [frame],
        ),
        createElement('span', { dataset: { part: 'label' } }, [label ?? 'Loading']),
      ],
    )
  }
})
