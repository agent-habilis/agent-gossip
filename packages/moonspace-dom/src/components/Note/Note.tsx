import { component } from 'visage-dom'
import type { Attrs, Child } from 'visage-dom/types'
import { Style, css, raw } from 'visage-style'
import { ONE_ROW, cells } from '../../mixins.ts'
import { t } from '../../tokens.ts'

export type NoteTone = 'info' | 'success' | 'warning' | 'danger'

export interface NoteProps extends Attrs<'div'> {
  tone?: NoteTone
  /** Overrides the default label for the tone. */
  label?: string
  children?: Child
}

/**
 * Each tone carries a marker and a word, not just a color. The word is what makes a
 * note readable when it is piped through a monochrome terminal or read aloud.
 */
const TONE: Record<NoteTone, { marker: string; label: string }> = {
  info: { marker: 'i', label: 'note' },
  success: { marker: '✓', label: 'ok' },
  warning: { marker: '!', label: 'warn' },
  danger: { marker: '✗', label: 'error' },
}

/**
 * One hoisted stylesheet for all four tones.
 *
 * Unlike a colour *role* — sixteen of them, and any of them — the tones are a
 * closed set of four, so they are selectable by `data-tone` and belong in the
 * sheet. Each block sets nothing but `--ms-note-color`; the rules above read it
 * and never learn which tone they are painting. That is four one-line blocks
 * instead of four copies of the marker, label and rule declarations.
 *
 * Order is load-bearing. The base and the parts come first, then the variants,
 * so the cascade resolves ties on source order the way it is written here.
 */
const NOTE = css({
  display: 'flex',
  gap: cells(1),
  // A multi-value `padding` shorthand is not in the value grammar, and it does
  // not need to be: the note pads horizontally and never vertically.
  paddingInline: cells(1),
  background: t.bgSunken,

  /*
   * A single rule down the left edge, one cell out. This is the browser
   * equivalent of prefixing every line with '│' — it marks the block without
   * boxing it, so the note costs no extra rows above or below.
   */
  boxShadow: 'inset var(--ms-border-width) 0 0 0 var(--ms-note-color)',

  '[data-part="marker"]': {
    ...ONE_ROW,
    flex: 'none',
    width: cells(1),
    color: raw('var(--ms-note-color)'),
  },

  '[data-part="label"]': {
    ...ONE_ROW,
    flex: 'none',
    textTransform: 'uppercase',
    color: raw('var(--ms-note-color)'),
  },

  // Without this the body refuses to shrink below its longest word and pushes
  // the note past its container — the flexbox min-content floor.
  '[data-part="body"]': { minWidth: 0 },

  '&[data-tone="info"]': { '--ms-note-color': t.info },
  '&[data-tone="success"]': { '--ms-note-color': t.success },
  '&[data-tone="warning"]': { '--ms-note-color': t.warning },
  '&[data-tone="danger"]': { '--ms-note-color': t.danger },
})

/**
 * A callout for a short piece of advice or a warning.
 *
 * Grows in whole rows as its content wraps, and adds none of its own — no vertical
 * padding, no border rows. A note in a dense list of them stays dense.
 */
export const Note = component<NoteProps>(function* (props) {
  yield () => {
    // Destructured inside the thunk, not in the parameter list — `props` is a
    // live tracking proxy and destructuring in the signature throws.
    const { tone = 'info', label, children, ...rest } = props
    const { marker, label: fallback } = TONE[tone]

    return (
      <div {...rest} dataset={{ tone }}>
        {Style(NOTE)}
        <span dataset={{ part: 'marker' }} attrs={{ 'aria-hidden': 'true' }}>
          {marker}
        </span>
        <span dataset={{ part: 'label' }}>{label ?? fallback}</span>
        <div dataset={{ part: 'body' }}>{children}</div>
      </div>
    )
  }
})
