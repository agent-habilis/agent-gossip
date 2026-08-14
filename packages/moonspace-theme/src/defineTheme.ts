import type { ColorRole, ColorValue } from './roles.ts'

/**
 * A palette: one set of answers to the questions `COLOR_ROLES` asks.
 *
 * Web-only, and there is no terminal counterpart — see `ansi.ts` for why the
 * terminal takes its colours from the user rather than from here.
 *
 * `appearance` is not decoration. `moonspace-dom` pairs a light and a dark theme
 * into a single `light-dark()` token set, and that pairing is only meaningful if
 * each side knows which it is.
 */
export interface ColorTheme {
  readonly name: string
  readonly appearance: 'dark' | 'light'
  readonly color: Readonly<Record<ColorRole, ColorValue>>
}

/**
 * Declare a theme, checked against the contract.
 *
 * The built-in themes use `as const satisfies ColorTheme` directly, which is
 * strictly better for them: it checks the contract *and* keeps every hex at its
 * literal type, which the swatch stories and the contrast test both read.
 *
 * This exists for the case that motivated the package — someone assembling a
 * theme at a call site, where `satisfies` would need them to import the type and
 * spell the annotation. Passing through a `const` type parameter keeps the
 * literals that a plain `const` would widen away.
 */
export const defineTheme = <const T extends ColorTheme>(theme: T): T => theme
