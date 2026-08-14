import { component } from 'visage-dom'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { glyphs } from 'moonspace'
import type { ColorRole } from 'moonspace-theme'
import { withStyle } from '../../internal.ts'
import { ONE_ROW, SR_ONLY, cells } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export type Status = 'ready' | 'building' | 'error' | 'queued' | 'canceled'

export interface StatusDotProps extends Attrs<'span'> {
  status: Status
  /** Optional text after the marker. Without it, `status` becomes the accessible name. */
  children?: Child
}

/**
 * Each status gets its own glyph as well as its own color.
 *
 * Deliberately redundant: on a 16-color terminal `warning` and `error` can collapse
 * onto the same slot, and in monochrome every one of them collapses. The glyph is
 * what keeps them distinguishable, so it — not the color — is the primary signal.
 * `fgMuted` and `fgSubtle` are the sharpest case: both are `brightBlack` in ANSI-16,
 * so `queued` and `canceled` differ by shape alone the moment the palette narrows.
 *
 * The glyphs themselves live in `theme/glyphs.ts` alongside the rest of the
 * vocabulary, so the TUI renderer inherits the same scale without importing this.
 */
const STATUS: Record<Status, { color: ColorRole; glyph: string }> = {
  ready: { color: 'success', glyph: glyphs.status.ready },
  building: { color: 'warning', glyph: glyphs.status.building },
  error: { color: 'danger', glyph: glyphs.status.error },
  queued: { color: 'fgMuted', glyph: glyphs.status.queued },
  canceled: { color: 'fgSubtle', glyph: glyphs.status.canceled },
}

/**
 * One hoisted stylesheet, with the marker's colour read from a custom property.
 *
 * Five statuses would be five near-identical blocks differing in a single
 * declaration, and the glyph already has to be picked in TypeScript — so the
 * colour is picked beside it and handed over as `--ms-status-color`.
 * `data-status` still lands on the element, but as a hook for callers and
 * devtools rather than as the selector this sheet turns on.
 *
 * A selector that does not start with `&` compounds as a descendant, so
 * `[data-part="marker"]` compiles to `:scope [data-part="marker"]`. The parts are
 * addressed by role rather than by tag, which leaves the markup free to change.
 */
const STATUS_DOT = css({
  ...ONE_ROW,
  display: 'inline-flex',
  alignItems: 'center',
  gap: cells(1),

  '[data-part="marker"]': {
    // A fixed cell, so a label starts on the same column whichever status shows.
    flex: 'none',
    width: cells(1),
    color: raw('var(--ms-status-color)'),
  },

  '[data-part="sr-label"]': { ...SR_ONLY },
})

/**
 * A one-cell status marker, optionally followed by a label.
 */
export const StatusDot = component<StatusDotProps>(function* (props) {
  yield () => {
    /*
     * Destructured here rather than in the parameter list: `props` is a live
     * tracking proxy, and destructuring in the signature throws.
     */
    const { status, children, style, ...rest } = props
    const { color, glyph } = STATUS[status]

    return (
      <span
        {...rest}
        dataset={{ status }}
        style={withStyle(style, { '--ms-status-color': String(t[color]) })}
      >
        {Style(STATUS_DOT)}
        <span dataset={{ part: 'marker' }} attrs={{ 'aria-hidden': 'true' }}>
          {glyph}
        </span>
        {/*
         * Without a visible label the glyph alone means nothing to a screen
         * reader, so the status word stands in — hidden from view, still in the
         * accessibility tree.
         */}
        {children != null ? (
          <span>{children}</span>
        ) : (
          <span dataset={{ part: 'sr-label' }}>{status}</span>
        )}
      </span>
    )
  }
})
