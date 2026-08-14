/**
 * moonspace, rendered to the DOM.
 *
 * The browser half of the system, built on visage-dom and visage-style —
 * generators and signals rather than React, and scoped `<style>` elements
 * rather than a CSS-in-JS runtime. `moonspace-tui` is the terminal half, and
 * both take their glyph vocabulary and grid from `moonspace` and their semantic
 * colour roles from `moonspace-theme` — plain data, neither knowing about a
 * renderer.
 *
 * The palette is a web concern and lives only on this side. `moonspace-tui`
 * ships none: it resolves every role to one of the sixteen ANSI slot names and
 * lets the user's terminal theme supply the values.
 *
 * Render `MoonspaceTheme()` and `GlobalStyle()` once at the app root: the first
 * sets the custom properties every component reads, the second is the reset, the
 * grid, and the `color-scheme` rules that pick between the light and dark halves
 * of each token.
 */

// Theme plumbing.
export { GlobalStyle } from './GlobalStyle.ts'
export { MoonspaceTheme, t } from './tokens.ts'
export type { MoonspaceThemeProps } from './tokens.ts'

// Grid math and shared style fragments, for anything built on top of this.
export {
  cells,
  rows,
  DISABLED,
  FOCUS_RING,
  INVERSE,
  ONE_ROW,
  SNAP,
  SNAP_MEASURE,
  SR_ONLY,
} from './mixins.ts'

// Primitives
export * from './components/Box/index.ts'
export * from './components/Text/index.ts'
export * from './components/Stack/index.ts'
export * from './components/Divider/index.ts'

// Controls
export * from './components/Button/index.ts'
export * from './components/Input/index.ts'
export * from './components/Select/index.ts'
export * from './components/Checkbox/index.ts'
export * from './components/Radio/index.ts'

// Display
export * from './components/Badge/index.ts'
export * from './components/StatusDot/index.ts'
export * from './components/Table/index.ts'
export * from './components/ProgressBar/index.ts'
export * from './components/Spinner/index.ts'
export * from './components/MiddleTruncate/index.ts'
export * from './components/Kbd/index.ts'

// Feedback and navigation
export * from './components/Note/index.ts'
export * from './components/Tabs/index.ts'
