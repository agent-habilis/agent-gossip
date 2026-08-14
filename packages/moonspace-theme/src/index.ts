/**
 * moonspace-theme — the colour layer, as swappable data.
 *
 * Split out of `moonspace` so a palette is a dependency you choose rather than
 * a decision the design system made for you. Nothing here imports a renderer,
 * or anything at all: `moonspace-dom` turns a `ColorTheme` into CSS custom
 * properties and `moonspace-tui` reads `ansi` and never sees a hex.
 *
 * Two halves, and the asymmetry is the design:
 *
 * - **The web gets palettes.** `superstylinDark` and `superstylinLight` are the
 *   defaults; `defineTheme()` is how you supply your own. A browser has no
 *   opinion about colour, so the system has to bring one.
 * - **The terminal gets names.** `ansi` maps every role onto one of the sixteen
 *   slots, and the values come from the user's terminal theme — which they
 *   already chose, and which is already right for their background.
 *
 * The constraint that keeps the two honest: every role in `COLOR_ROLES` has an
 * entry in `ansi`. A colour that cannot survive sixteen slots does not belong in
 * the system, and since ANSI-16 is the terminal's only rendering path, that is
 * checked by every terminal render rather than by a flag someone remembers to
 * flip.
 */

export { COLOR_ROLES } from './roles.ts'
export type { AnsiColor, ColorRole, ColorValue } from './roles.ts'

export { ansi } from './ansi.ts'

export { defineTheme } from './defineTheme.ts'
export type { ColorTheme } from './defineTheme.ts'

export {
  superstylinDark,
  superstylinLight,
  superstylinPalette,
  superstylinPaletteValues,
} from './themes/superstylin.ts'

export {
  CHROME_ROLES,
  CONTRAST_FLOOR,
  SURFACE_ROLES,
  TEXT_ROLES,
  contrast,
  luminance,
  parseHex,
} from './contrast.ts'
