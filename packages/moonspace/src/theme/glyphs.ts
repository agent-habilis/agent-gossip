/**
 * The glyph vocabulary.
 *
 * Every piece of component chrome in this system is a character, not a graphic.
 * No SVG icons, no CSS-drawn shapes, no images. If it can't be typed into a
 * terminal, it isn't in here.
 *
 * Centralising them has a second purpose: this file is the complete inventory of
 * what the future TUI renderer has to be able to draw. Adding a glyph anywhere
 * else in the codebase hides that cost.
 *
 * Every glyph below occupies exactly one cell in a browser, and every one has been
 * measured to confirm it. Emoji and other unambiguously double-width characters are
 * banned outright — they break the grid here and wreck column alignment in a
 * terminal.
 *
 * One caveat for the TUI port: the geometric shapes and circled operators used for
 * status (● ◐ ⊗ ◌ ⊘) are East Asian Width **Ambiguous**, not Narrow. They render one
 * column wide under the usual terminal defaults, but a terminal configured with
 * ambiguous-width=double will give them two. The renderer should either pin that
 * setting or substitute single-width ASCII markers.
 */

/** Box-drawing sets, one per border weight. */
export const boxDrawing = {
  line: {
    topLeft: '┌', top: '─', topRight: '┐',
    left: '│', right: '│',
    bottomLeft: '└', bottom: '─', bottomRight: '┘',
    teeLeft: '├', teeRight: '┤', teeTop: '┬', teeBottom: '┴', cross: '┼',
  },
  double: {
    topLeft: '╔', top: '═', topRight: '╗',
    left: '║', right: '║',
    bottomLeft: '╚', bottom: '═', bottomRight: '╝',
    teeLeft: '╠', teeRight: '╣', teeTop: '╦', teeBottom: '╩', cross: '╬',
  },
  thick: {
    topLeft: '┏', top: '━', topRight: '┓',
    left: '┃', right: '┃',
    bottomLeft: '┗', bottom: '━', bottomRight: '┛',
    teeLeft: '┣', teeRight: '┫', teeTop: '┳', teeBottom: '┻', cross: '╋',
  },
} as const

export type BorderWeight = keyof typeof boxDrawing

/** Control affordances. Each pair must differ in shape, never in color alone. */
export const glyphs = {
  /** Checkbox: unchecked, checked, indeterminate. */
  checkbox: { off: ' ', on: '×', mixed: '-' },
  /** Radio: unselected, selected. */
  radio: { off: ' ', on: '•' },
  /** Disclosure arrows. */
  chevron: { down: '▾', up: '▴', right: '▸', left: '◂' },
  /** Progress bar fill and track. */
  bar: { fill: '█', half: '▌', empty: '░' },
  /**
   * Status markers.
   *
   * All one family — a filled circle, progressively emptied, struck out or voided —
   * so the set reads as a single scale rather than five unrelated symbols. Each is a
   * distinct *shape*, not just a distinct color: `ready` and `error` would be the
   * same glyph if error were a plain red dot, and would become indistinguishable the
   * moment the marker is rendered without its label or on a monochrome terminal.
   */
  status: {
    ready: '●',
    building: '◐',
    error: '⊗',
    queued: '◌',
    canceled: '⊘',
  },
  /** Ellipsis. One character, not three periods — see MiddleTruncate. */
  ellipsis: '…',
  /** Text cursor / caret. */
  caret: '▏',
  /** Prompt marker. */
  prompt: '>',
  /** Breadcrumb and path separators. */
  separator: { path: '/', crumb: '›' },
  /** Bullet for unordered lists. */
  bullet: '·',
} as const

/**
 * Spinner frames. Braille cells animate smoothly in a single column and are
 * available in every font we fall back to.
 */
export const spinnerFrames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'] as const

/** An ASCII-only spinner, for terminals without reliable Braille coverage. */
export const asciiSpinnerFrames = ['|', '/', '-', '\\'] as const

/**
 * ASCII fallbacks for everything above that is not plain ASCII already.
 *
 * The status markers and the bar are the glyphs the East Asian Width note
 * applies to, so a renderer that has decided it cannot trust the terminal's font
 * needs somewhere to fall back to — otherwise "ascii mode" converts the borders
 * and leaves the riskiest characters in place.
 *
 * The shapes still differ from each other, which is the property that matters:
 * `*` `o` `x` `.` `-` are as distinguishable as `● ◐ ⊗ ◌ ⊘`, just uglier.
 */
export const asciiGlyphs = {
  status: {
    ready: '*',
    building: 'o',
    error: 'x',
    queued: '.',
    canceled: '-',
  },
  bar: { fill: '#', half: '=', empty: '.' },
  chevron: { down: 'v', up: '^', right: '>', left: '<' },
  ellipsis: '...',
  bullet: '*',
} as const
