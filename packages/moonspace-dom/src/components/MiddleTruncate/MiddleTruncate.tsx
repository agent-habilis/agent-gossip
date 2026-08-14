import { component, signal } from 'visage-dom'
import { createElement } from 'visage-dom/element'
import type { Attrs } from 'visage-dom/types'
import { Style, css } from 'visage-style'
// Pure and renderer-agnostic, which is why it lives in `moonspace` rather than
// beside this component: a terminal already knows its width in columns and can
// call it directly, with none of the measurement below.
import { middleTruncate } from 'moonspace'

export interface MiddleTruncateProps extends Attrs<'span'> {
  /** The string to truncate. */
  value: string
  /**
   * Fix the character budget instead of measuring the container. Useful when the
   * width is already known — a table cell of a declared width, for instance.
   */
  budget?: number
}

const MIDDLE_TRUNCATE = css({
  display: 'block',
  minWidth: 0,
  /*
   * No text-overflow: ellipsis here, and none should be added by a parent. CSS
   * ellipsis clips the end of the string, which is exactly the half this
   * component exists to preserve; running both means the browser truncates our
   * truncation.
   */
  overflow: 'hidden',
  whiteSpace: 'nowrap',
})

/** How many characters the probe lays out. See `measure`. */
const RUN = 100

/**
 * Truncates in the middle, keeping the start and the end.
 *
 * For values where both ends carry meaning — file paths, URLs, deployment IDs,
 * commit hashes, prefixed branch names — end-truncation throws away the part
 * that actually identifies the thing. `~/dev/…/Button.tsx` is useful;
 * `~/dev/personal/moonspa…` is not.
 *
 * Uses the single `…` character rather than three periods: three periods cost
 * three cells in a monospace font where one will do, and the wider gap reads as
 * an intentional pause in the string rather than as elision.
 *
 * The full value is always on `title`, so the untruncated string stays
 * reachable.
 *
 * The React version was a component over a 91-line hook — `useRef` plus
 * `useState` plus `useCallback` plus a `useLayoutEffect` for the observer and a
 * `useEffect` for the font swap. What is left is one signal and one `ref`: a
 * visage `ref` runs on mount and the function it returns runs on unmount, so
 * the subscription and its teardown are one expression and there is no
 * dependency array to keep honest.
 */
export const MiddleTruncate = component<MiddleTruncateProps>(function* (props) {
  /** Null until the first measurement lands, so the full value renders meanwhile. */
  const budget = signal<number | null>(null)

  /*
   * Monospace makes this exact and cheap. Rather than measuring candidate
   * substrings or binary-searching for a fit, we measure one character and
   * divide — every character has the same advance width, so the answer is
   * arithmetic. That is a property this system gets for free and a proportional
   * font never could.
   *
   * A run of `RUN` characters rather than one, so rounding error in the
   * reported advance width is divided away rather than compounded.
   */
  const measure = (el: HTMLSpanElement): void => {
    const probe = document.createElement('span')
    probe.style.cssText =
      'position:absolute;visibility:hidden;white-space:pre;font:inherit;letter-spacing:inherit;'
    probe.textContent = '0'.repeat(RUN)
    el.appendChild(probe)
    const cell = probe.getBoundingClientRect().width / RUN
    probe.remove()

    if (cell <= 0) return
    budget.value = Math.max(0, Math.floor(el.getBoundingClientRect().width / cell))
  }

  const attach = (el: HTMLSpanElement) => {
    measure(el)
    const observer = new ResizeObserver(() => measure(el))
    observer.observe(el)
    /*
     * Re-measure once webfonts settle. TX-02 and the fallback have different
     * advance widths, so a budget computed before the swap is simply wrong —
     * and the swap does not necessarily change the element's own width, so the
     * ResizeObserver may never fire for it. `this.aborted` is the component's
     * own unmount signal, which is what the hook's `cancelled` flag was standing
     * in for.
     */
    void document.fonts?.ready.then(() => {
      if (!this.aborted.aborted) measure(el)
    })
    return () => observer.disconnect()
  }

  /*
   * Whether to measure at all is decided here, once, because a visage `ref`
   * runs at mount and never again — there is no second chance to attach the
   * observer. A `budget` that appears after mount is still honoured by the
   * thunk below; it just leaves an observer running that nothing reads.
   */
  const fixed = props.budget !== undefined

  yield () => {
    const { value, budget: given, ...rest } = props
    const fit = given ?? budget.value

    return createElement(
      'span',
      { ...rest, ref: fixed ? undefined : attach, title: value },
      [Style(MIDDLE_TRUNCATE), fit === null ? value : middleTruncate(value, fit)],
    )
  }
})
