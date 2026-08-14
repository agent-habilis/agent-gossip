import type { ColorTheme } from '../defineTheme.ts'

/**
 * Superstylin — the default palette, in a dark and a light variant.
 *
 * Adapted from https://github.com/caiogondim/superstylin, a drop-in CSS library
 * whose whole colour system is a near-neutral ramp plus two hues. The ramp has a
 * quiet signature worth preserving: every step has green one below red and blue,
 * so the greys are a degree warmer than true neutral.
 *
 * Two things had to be added rather than transcribed.
 *
 * Superstylin has no success, warning, info or secondary accent — it never needs
 * them, being a stylesheet for documents. Those four are new, designed to sit at
 * the same luminance as superstylin's own blue and red so the set reads as one
 * family.
 *
 * And its two hues are tuned as *borders and rings* — a focus outline, an
 * `:invalid` edge — where moonspace uses these roles as text. `#5aaafa` on white
 * is 2.8:1, which is fine for a 2px ring and unreadable as a word. So the light
 * variant darkens every hue; only the mid-ramp neutrals are shared between the
 * two, and `theme.test.ts` holds all of them to a contrast floor.
 */

/**
 * The raw ramp, transcribed from superstylin's `src/index.css`.
 *
 * Components must never import this. Use the roles below — the indirection is
 * what lets a palette be swapped without touching a component, and it is also
 * what the "every role traces to a palette entry" test checks.
 */
const ramp = {
  white: '#ffffff',
  gray1: '#f9f9f9',
  gray2: '#f6f6f6',
  gray3: '#d8d8d8',
  gray4: '#c0bfc0',
  gray5: '#a6a5a6',
  gray6: '#949394',
  gray7: '#777677',
  gray8: '#595859',
  gray9: '#343334',
  black: '#272727',
  /** One step below `black`, for a surface that has to recede on a dark page. */
  blackSunken: '#1e1d1e',
  /**
   * The seam. Below every surface in either theme, which is the whole job.
   *
   * Pure black rather than a near-black because `#272727` leaves only about 0.02
   * of luminance beneath it, and black is the only value there that matches the
   * separation the light theme already gets from `gray3` — 1.41:1 against `bg`
   * versus 1.43:1. Anything lighter is a seam in the dark theme and a suggestion
   * of one.
   */
  seam: '#000000',
} as const

/** Superstylin's own two hues, plus four designed to match them. */
const hueDark = {
  blue: '#5aaafa',
  red: '#ff7d87',
  purple: '#c99cf5',
  green: '#6cd8a0',
  amber: '#f5b455',
  cyan: '#5ad0e8',
  /** A dark tint of blue, for selection. */
  blueSunken: '#2f4a63',
} as const

/** The same six, darkened to carry text on white. */
const hueLight = {
  blue: '#0f62c8',
  red: '#c62435',
  purple: '#7b3fc4',
  green: '#127a4a',
  amber: '#8a5a00',
  cyan: '#0e6d80',
  /** A light tint of blue, for selection. */
  blueRaised: '#d6e8fb',
} as const

export const superstylinDark = {
  name: 'superstylin-dark',
  appearance: 'dark',
  color: {
    bg: ramp.black,
    bgSunken: ramp.blackSunken,
    bgRaised: ramp.gray9,
    bgSelected: hueDark.blueSunken,

    fg: ramp.white,
    fgMuted: ramp.gray5,
    fgSubtle: ramp.gray7,
    fgOnAccent: ramp.black,

    border: ramp.seam,
    borderStrong: ramp.gray6,

    accent: hueDark.blue,
    accentAlt: hueDark.purple,

    success: hueDark.green,
    warning: hueDark.amber,
    danger: hueDark.red,
    info: hueDark.cyan,
  },
} as const satisfies ColorTheme

/**
 * The light variant.
 *
 * `fgSubtle` and `borderStrong` are the same values as the dark theme's, which
 * is not laziness — `#777677` and `#949394` sit near enough the middle of the
 * ramp to clear their contrast targets against both `#272727` and `#ffffff`. A
 * small piece of evidence that superstylin's ramp is well centred.
 *
 * `border` needed no change here. `gray3` was already below every surface in
 * this theme, which is what the dark variant had to be fixed to match — see
 * `ramp.seam`, and the `border sits below every surface` test that now holds
 * both of them to it.
 *
 * `bgRaised` is the one compromise in the set. On a white page a raised surface
 * cannot go lighter than the page, so raised and sunken both read as off-white
 * and the difference between them nearly vanishes. That is tolerable here for
 * the same reason the ANSI mapping collapses them onto one slot: this system
 * already forbids signalling depth by colour alone.
 */
export const superstylinLight = {
  name: 'superstylin-light',
  appearance: 'light',
  color: {
    bg: ramp.white,
    bgSunken: ramp.gray2,
    bgRaised: ramp.gray1,
    bgSelected: hueLight.blueRaised,

    fg: ramp.black,
    fgMuted: ramp.gray8,
    fgSubtle: ramp.gray7,
    fgOnAccent: ramp.white,

    border: ramp.gray3,
    borderStrong: ramp.gray6,

    accent: hueLight.blue,
    accentAlt: hueLight.purple,

    success: hueLight.green,
    warning: hueLight.amber,
    danger: hueLight.red,
    info: hueLight.cyan,
  },
} as const satisfies ColorTheme

/**
 * The raw palette, in its three groups.
 *
 * Grouped rather than flattened because the two hue sets share key names — a
 * spread would silently drop one `blue` on the floor — and because the swatch
 * story wants to show the neutral ramp and the two hue sets as three separate
 * runs anyway.
 */
export const superstylinPalette = {
  ramp,
  dark: hueDark,
  light: hueLight,
} as const

/** Every value in the palette, for the "no literal hexes in a role" test. */
export const superstylinPaletteValues: readonly string[] = [
  ...Object.values(ramp),
  ...Object.values(hueDark),
  ...Object.values(hueLight),
]
