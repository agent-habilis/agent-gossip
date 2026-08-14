/**
 * moonspace — the shared core of a monospace design system.
 *
 * One font, one size, one grid.
 *
 * This package is plain data and pure functions: the grid, the glyph vocabulary
 * and the typography stack. It knows about no renderer, which is the point —
 * `moonspace-dom` draws it with scoped CSS and `moonspace-tui` draws it with
 * characters and ANSI escapes, and the two agree because they read the same
 * numbers.
 *
 * Colour is deliberately not here. It lives in `moonspace-theme`, a sibling
 * package with no dependency in either direction, because a palette is the one
 * part of this system you are most likely to want to replace — and because the
 * two renderers do not want the same thing from it. The web needs hexes; the
 * terminal needs the sixteen slot names and takes its values from the user's own
 * terminal theme.
 */

export * from './theme/index.ts'
export { middleTruncate } from './truncate.ts'
