import { component } from 'visage-dom'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import type { ColorRole } from 'moonspace-theme'
import { withStyle } from '../../internal.ts'
import { ONE_ROW } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export type BadgeTone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger' | 'info'

export interface BadgeProps extends Attrs<'span'> {
  tone?: BadgeTone
  /**
   * `solid` fills the badge and inverts the text — the loudest thing the system can
   * do. `outline` brackets the label instead, for when several badges sit together
   * and a row of solid blocks would be overwhelming.
   */
  variant?: 'solid' | 'outline'
  children?: Child
}

/**
 * Tone is a badge-level word, not a palette one — `neutral` is what a caller
 * asks for, `fgMuted` is what the theme calls it. Keeping the indirection means
 * the six tones can be re-pointed without touching a call site.
 */
const TONE: Record<BadgeTone, ColorRole> = {
  neutral: 'fgMuted',
  accent: 'accent',
  success: 'success',
  warning: 'warning',
  danger: 'danger',
  info: 'info',
}

/**
 * One hoisted stylesheet, with the tone carried in on `--ms-badge-tone`.
 *
 * Six tones crossed with two variants would be twelve blocks, eleven of which
 * differ only in one colour. A custom property collapses them to two: the base
 * paints the text with it, and `solid` paints the background with the same
 * value. The role name never reaches the CSS.
 *
 * Order is load-bearing — `solid` overrides the base `color`, and it can only do
 * that by appearing after it.
 */
const BADGE = css({
  ...ONE_ROW,
  display: 'inline-flex',
  alignItems: 'center',
  whiteSpace: 'nowrap',
  textTransform: 'uppercase',
  color: raw('var(--ms-badge-tone)'),

  '&[data-variant="solid"]': {
    /* One cell of padding each side keeps the filled block on whole cells. */
    paddingInline: '1ch',
    background: raw('var(--ms-badge-tone)'),
    color: t.bg,
  },

  /*
   * Two literal characters rather than a border or a ring. A `border`
   * participates in layout and would inflate the line box of whatever text the
   * badge sits in — a border-free treatment is a hard requirement for an inline
   * chip, which is why the bracket glyphs are load-bearing and not decoration.
   * They also cost exactly one cell each, so an outline badge and a solid badge
   * of the same label are the same width.
   */
  '&[data-variant="outline"]::before': { content: "'‹'" },
  '&[data-variant="outline"]::after': { content: "'›'" },
})

/**
 * A compact status label.
 *
 * Both variants are terminal-native: `solid` is reverse video, `outline` is two
 * literal bracket characters. Neither depends on hue to read as a badge, which
 * matters because tone is the one thing here that *is* color-only.
 */
export const Badge = component<BadgeProps>(function* (props) {
  yield () => {
    // Destructured here rather than in the parameter list: `props` is a live
    // tracking proxy and destructuring in the signature throws.
    const { tone, variant, children, style, ...rest } = props

    return (
      <span
        {...rest}
        dataset={{ tone: tone ?? 'neutral', variant: variant ?? 'solid' }}
        /*
         * `t` is keyed by role, so `t[TONE[tone]]` is already the `var(--success)`
         * string the property needs — no name mangling, and a role that does not
         * exist is a compile error rather than an empty custom property.
         */
        style={withStyle(style, { '--ms-badge-tone': String(t[TONE[tone ?? 'neutral']]) })}
      >
        {/* First child, so the <style> takes the `:first-child` slot. */}
        {Style(BADGE)}
        {children}
      </span>
    )
  }
})
