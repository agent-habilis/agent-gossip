import type { ColorValue } from './roles.ts'

/**
 * WCAG contrast, in the ~30 lines it actually takes.
 *
 * Here rather than in a test helper because two callers need it and they must
 * not disagree: `theme.test.ts` holds every built-in theme to a floor, and the
 * Contrast story shows the same numbers to whoever is authoring a new one. A
 * palette that passes CI and looks failing on the page would be worse than
 * having neither.
 *
 * Hex only. `ColorValue` admits `oklch()` and friends, which would need a colour
 * space conversion to measure — `parseHex` returns `null` for those and callers
 * report "unmeasured" rather than guessing. Every built-in theme is hex.
 */

/** `#rgb`, `#rrggbb` and `#rrggbbaa` (alpha ignored). `null` if it is not a hex. */
export function parseHex(value: string): readonly [number, number, number] | null {
  const match = /^#([0-9a-f]{3,8})$/i.exec(value.trim())
  if (!match) return null

  const digits = match[1] as string
  let full: string
  if (digits.length === 3) full = digits.replace(/./g, (c) => c + c)
  else if (digits.length === 6 || digits.length === 8) full = digits.slice(0, 6)
  else return null

  const n = Number.parseInt(full, 16)
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff]
}

/** sRGB channel to linear light, per WCAG 2.x. */
const channel = (value: number): number => {
  const c = value / 255
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4
}

/** Relative luminance, 0 (black) to 1 (white). `null` for a non-hex colour. */
export function luminance(color: ColorValue | string): number | null {
  const rgb = parseHex(color)
  if (!rgb) return null
  return 0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

/**
 * Contrast ratio between two colours, 1 to 21. `null` if either is not a hex.
 *
 * Order-independent — the brighter of the two goes on top, which is what makes
 * `contrast(fg, bg)` and `contrast(bg, fg)` the same number.
 */
export function contrast(a: ColorValue | string, b: ColorValue | string): number | null {
  const la = luminance(a)
  const lb = luminance(b)
  if (la === null || lb === null) return null
  const [lighter, darker] = la >= lb ? [la, lb] : [lb, la]
  return (lighter + 0.05) / (darker + 0.05)
}

/**
 * The floors every built-in theme is held to, and the ones a custom theme
 * should be.
 *
 * `text` is WCAG AA for body copy. `chrome` is the 3:1 that AA asks of
 * non-text — `fgSubtle` is placeholders and disabled controls, `borderStrong`
 * is an edge, and neither is prose.
 */
export const CONTRAST_FLOOR = { text: 4.5, chrome: 3 } as const

/** Roles that carry words, measured against `bg`. */
export const TEXT_ROLES = [
  'fg',
  'fgMuted',
  'accent',
  'accentAlt',
  'success',
  'warning',
  'danger',
  'info',
] as const

/** Roles that draw chrome, measured against `bg`. */
export const CHROME_ROLES = ['fgSubtle', 'borderStrong'] as const

/**
 * Every surface a border can be drawn on.
 *
 * `border` is deliberately absent from `CHROME_ROLES`, and that is not an
 * oversight — it is not held to a contrast floor at all. It is a seam: a dark
 * gap between surfaces rather than a line on top of one, and a floor would be
 * asking it to be the opposite of what it is. In the dark theme it could not
 * meet one anyway, since `bg` leaves about 0.02 of luminance beneath it and pure
 * black tops out at 1.41:1.
 *
 * What it is held to instead is direction: below *every* surface, in every
 * theme. `theme.test.ts` checks it, which is what stops a palette from
 * reintroducing the lighter border this list was added to catch.
 */
export const SURFACE_ROLES = ['bg', 'bgSunken', 'bgRaised', 'bgSelected'] as const
