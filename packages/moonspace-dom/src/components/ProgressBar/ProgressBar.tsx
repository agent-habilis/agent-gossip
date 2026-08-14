import { component, signal } from 'visage-dom'
import type { Attrs } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { glyphs } from 'moonspace'
import type { ColorRole } from 'moonspace-theme'
import { withStyle } from '../../internal.ts'
import { ONE_ROW, cells } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export interface ProgressBarProps extends Attrs<'span'> {
  /** Progress from 0 to 1. Values outside the range are clamped. */
  value: number
  /** Track width in cells. */
  width?: number
  /**
   * Fill the container instead of drawing a fixed `width` of cells.
   * Vendor-local: not in upstream moonspace.
   */
  fluid?: boolean
  /** Append ` 67%` after the track. Costs 5 cells. */
  showValue?: boolean
  tone?: Extract<ColorRole, 'accent' | 'success' | 'warning' | 'danger'>
  /** Accessible name. Required — a bare bar tells a screen reader nothing. */
  label: string
}

/**
 * One hoisted stylesheet. Nothing here varies per instance except the tone,
 * which arrives as `--ms-progress-tone`.
 *
 * `white-space: pre` is load-bearing rather than cosmetic: the track is a run of
 * identical characters, and without it a browser would be free to collapse or
 * wrap them, which would silently change the length of the bar.
 */
const PROGRESS_BAR = css({
  ...ONE_ROW,
  display: 'inline-flex',
  alignItems: 'center',
  gap: cells(1),
  whiteSpace: 'pre',

  '[data-part="track"]': {
    flex: 'none',
    color: t.fgSubtle,
  },

  '[data-part="fill"]': {
    color: raw('var(--ms-progress-tone)'),
  },

  /*
   * Four cells and right-aligned, so the number's last digit sits on a fixed
   * column as it grows from 0% to 100% and the bar never shifts.
   */
  '[data-part="value"]': {
    flex: 'none',
    width: cells(4),
    textAlign: 'right',
    color: t.fgMuted,
  },

  /*
   * Fluid: the wrapper stretches, and the track takes the slack. Zero basis on
   * the track, so the measured width comes from the container rather than from
   * the glyphs already in it — otherwise every measurement would feed the next.
   */
  '&[data-fluid="true"]': {
    display: 'flex',
    flex: '1',
    minWidth: 0,

    '[data-part="track"]': {
      flex: '1',
      minWidth: 0,
      overflow: 'hidden',
    },
  },
})

/** How many characters the probe lays out. See `measure`. */
const RUN = 100

/**
 * A determinate progress bar built from `█` and `░`.
 *
 * The bar is real text, not a styled div: the fill is a run of block characters
 * whose length is computed in whole cells. That means the browser renders exactly
 * the string a terminal would print, and it degrades honestly — at 20 cells wide,
 * one cell is 5%, and the component rounds rather than pretending to sub-character
 * precision it cannot draw.
 */
export const ProgressBar = component<ProgressBarProps>(function* (props) {
  /** Track width in cells, once the element has been laid out. Fluid only. */
  const measured = signal<number | null>(null)

  /* Same probe as MiddleTruncate: measure one run of digits and divide. */
  const measure = (el: HTMLElement): void => {
    const probe = document.createElement('span')
    probe.style.cssText =
      'position:absolute;visibility:hidden;white-space:pre;font:inherit;letter-spacing:inherit;'
    probe.textContent = '0'.repeat(RUN)
    el.appendChild(probe)
    const cell = probe.getBoundingClientRect().width / RUN
    probe.remove()

    if (cell <= 0) return
    measured.value = Math.max(0, Math.floor(el.getBoundingClientRect().width / cell))
  }

  const attach = (el: HTMLElement) => {
    measure(el)
    const observer = new ResizeObserver(() => measure(el))
    observer.observe(el)
    // Re-measure once webfonts settle; the swap can change the advance width
    // without resizing the element, so the ResizeObserver may never fire.
    void document.fonts?.ready.then(() => {
      if (!this.aborted.aborted) measure(el)
    })
    return () => observer.disconnect()
  }

  /*
   * Whether to measure at all is decided here, once, because a visage `ref`
   * runs at mount and never again — there is no second chance to attach the
   * observer.
   */
  const fluid = props.fluid === true

  yield () => {
    // Destructured inside the thunk, not in the parameter list — `props` is a
    // live tracking proxy and destructuring in the signature throws.
    const {
      value,
      width = 24,
      fluid: _fluid,
      showValue = true,
      tone = 'accent',
      label,
      attrs,
      style,
      dataset,
      ...rest
    } = props

    const clamped = Math.min(1, Math.max(0, value))
    // Before the first fluid measurement lands there is nothing to draw — the
    // ResizeObserver fires on the very next frame.
    const trackCells = fluid ? (measured.value ?? 0) : width
    const filled = Math.round(clamped * trackCells)
    const percent = Math.round(clamped * 100)

    return (
      <span
        {...rest}
        dataset={{ ...dataset, fluid: fluid ? 'true' : 'false' }}
        /*
         * ARIA goes through `attrs` and every value is a string. `dataset` would
         * be wrong here — and `attrs` is what removes an attribute outright on a
         * boolean `false`, which is why the numbers are spelled out. A caller's
         * own `attrs` are merged under these rather than replaced, so
         * `aria-describedby` survives while `role` cannot be argued with.
         */
        attrs={{
          ...attrs,
          role: 'progressbar',
          'aria-label': label,
          'aria-valuenow': String(percent),
          'aria-valuemin': '0',
          'aria-valuemax': '100',
        }}
        style={withStyle(style, {
          '--ms-progress-tone': String(t[tone]),
          // The value as CSS sees it. The sheet above draws with characters and
          // does not read it; it is here so a caller styling around the bar has
          // the same number the ARIA does, without re-deriving it.
          '--ms-progress-value': `${percent}%`,
        })}
      >
        {Style(PROGRESS_BAR)}
        {/* The whole track is decoration — the value reaches AT through ARIA. */}
        <span
          dataset={{ part: 'track' }}
          attrs={{ 'aria-hidden': 'true' }}
          ref={fluid ? attach : undefined}
        >
          <span dataset={{ part: 'fill' }}>{glyphs.bar.fill.repeat(filled)}</span>
          {glyphs.bar.empty.repeat(trackCells - filled)}
        </span>
        {showValue ? (
          <span dataset={{ part: 'value' }} attrs={{ 'aria-hidden': 'true' }}>
            {`${percent}%`}
          </span>
        ) : null}
      </span>
    )
  }
})
