import { expect, test } from 'bun:test'
import { ansi } from './ansi.ts'
import { COLOR_ROLES } from './roles.ts'
import type { ColorRole } from './roles.ts'
import type { ColorTheme } from './defineTheme.ts'
import {
  CHROME_ROLES,
  CONTRAST_FLOOR,
  SURFACE_ROLES,
  TEXT_ROLES,
  contrast,
  luminance,
  parseHex,
} from './contrast.ts'
import {
  superstylinDark,
  superstylinLight,
  superstylinPaletteValues,
} from './themes/superstylin.ts'

/*
 * The colour layer is plain data with no renderer attached, which is the whole
 * reason both moonspace-dom and moonspace-tui can import it. These tests pin the
 * invariants that make that true — the ones a renderer would otherwise
 * rediscover as a bug.
 */

/** Every built-in theme. Add a theme here and it inherits the whole suite. */
const THEMES: ColorTheme[] = [superstylinDark, superstylinLight]

test('every role has an ANSI counterpart', () => {
  /*
   * The portability constraint, and since ANSI-16 is the terminal's only
   * rendering path, a missing entry is a role that renders as nothing rather
   * than a role that degrades. Checked against COLOR_ROLES rather than against
   * a theme's keys, so no single palette gets to define the contract.
   */
  expect(Object.keys(ansi).sort()).toEqual([...COLOR_ROLES].sort())
})

test('the ANSI map is appearance-agnostic', () => {
  /*
   * `default` is the terminal's own foreground/background — the only value that
   * is right in both a light and a dark terminal. If any of these three ever
   * becomes a concrete colour, the terminal has silently taken a side about the
   * user's background, which is the bug this whole mapping exists to avoid.
   */
  expect(ansi.bg).toBe('default')
  expect(ansi.fg).toBe('default')
  expect(ansi.borderStrong).toBe('default')
})

for (const theme of THEMES) {
  test(`${theme.name}: every role has a value, and only known roles`, () => {
    expect(Object.keys(theme.color).sort()).toEqual([...COLOR_ROLES].sort())
  })

  test(`${theme.name}: every role resolves to a palette entry, not a literal`, () => {
    // The indirection themes/superstylin.ts documents: a role names a ramp step,
    // so the ramp can be retuned in one place.
    const known = new Set<string>(superstylinPaletteValues)
    for (const role of COLOR_ROLES) {
      const value = theme.color[role]
      expect(parseHex(value), `${role} is not a hex`).not.toBeNull()
      expect(known.has(value), `${role} (${value}) is not a palette value`).toBe(true)
    }
  })

  test(`${theme.name}: text roles clear ${CONTRAST_FLOOR.text}:1 against bg`, () => {
    /*
     * The guardrail that makes the palette trustworthy rather than eyeballed —
     * and the one a custom theme most needs, since superstylin's own blue and
     * red were designed as a focus ring and an :invalid edge, where 2.8:1 is
     * fine and unreadable as a word.
     */
    for (const role of TEXT_ROLES) {
      const ratio = contrast(theme.color[role], theme.color.bg)
      expect(ratio, `${role} is not measurable`).not.toBeNull()
      expect(ratio as number).toBeGreaterThanOrEqual(CONTRAST_FLOOR.text)
    }
  })

  test(`${theme.name}: chrome roles clear ${CONTRAST_FLOOR.chrome}:1 against bg`, () => {
    for (const role of CHROME_ROLES) {
      const ratio = contrast(theme.color[role], theme.color.bg)
      expect(ratio as number).toBeGreaterThanOrEqual(CONTRAST_FLOOR.chrome)
    }
  })

  test(`${theme.name}: border sits below every surface`, () => {
    /*
     * A border is a seam — a dark gap between surfaces — not a line drawn on top
     * of one. Direction rather than ratio, because the ratio is deliberately
     * tiny: in a dark theme `bg` leaves about 0.02 of luminance beneath it, so
     * the whole available range is roughly 1.4:1.
     *
     * This is the check the dark palette failed. `border` was `#595859`, lighter
     * than all four of its surfaces, and nothing caught it: `CHROME_ROLES` gated
     * `borderStrong`, which renders nowhere, and left `border` — every divider,
     * every Box edge, the table rule, the scrollbar, the Input outline —
     * unchecked.
     */
    const border = luminance(theme.color.border)
    expect(border, 'border is not measurable').not.toBeNull()
    for (const role of SURFACE_ROLES) {
      const surface = luminance(theme.color[role])
      expect(surface, `${role} is not measurable`).not.toBeNull()
      expect(
        border as number,
        `border (${theme.color.border}) is not below ${role} (${theme.color[role]})`,
      ).toBeLessThan(surface as number)
    }
  })

  test(`${theme.name}: fgOnAccent is readable on accent`, () => {
    // The one pair that is not measured against `bg`: an accent fill is its own
    // background, so `fg` tells you nothing about whether the label on it reads.
    const ratio = contrast(theme.color.fgOnAccent, theme.color.accent)
    expect(ratio as number).toBeGreaterThanOrEqual(CONTRAST_FLOOR.text)
  })

  test(`${theme.name}: fg is readable on bgSelected`, () => {
    const ratio = contrast(theme.color.fg, theme.color.bgSelected)
    expect(ratio as number).toBeGreaterThanOrEqual(CONTRAST_FLOOR.text)
  })
}

test('the two superstylin variants declare opposite appearances', () => {
  // moonspace-dom pairs them into one light-dark() token set, which is only
  // meaningful if each side knows which it is.
  expect(superstylinDark.appearance).toBe('dark')
  expect(superstylinLight.appearance).toBe('light')
})

test('the light theme is actually lighter', () => {
  const dark = contrast(superstylinDark.color.bg, '#000000') as number
  const light = contrast(superstylinLight.color.bg, '#000000') as number
  expect(light).toBeGreaterThan(dark)
})

test('contrast is order-independent and bounded', () => {
  const role: ColorRole = 'fg'
  const forward = contrast(superstylinDark.color[role], superstylinDark.color.bg)
  const backward = contrast(superstylinDark.color.bg, superstylinDark.color[role])
  expect(forward).toBeCloseTo(backward as number, 10)
  expect(contrast('#000000', '#ffffff')).toBeCloseTo(21, 10)
  expect(contrast('#123456', '#123456')).toBeCloseTo(1, 10)
})

test('parseHex handles the three hex lengths and rejects the rest', () => {
  expect(parseHex('#fff')).toEqual([255, 255, 255])
  expect(parseHex('#ffffff')).toEqual([255, 255, 255])
  // Alpha is parsed off and ignored rather than rejected.
  expect(parseHex('#ffffff80')).toEqual([255, 255, 255])
  expect(parseHex('oklch(0.7 0.1 200)')).toBeNull()
  expect(parseHex('#gg0000')).toBeNull()
  // A non-hex colour is unmeasured, not silently scored.
  expect(contrast('oklch(0.7 0.1 200)', '#ffffff')).toBeNull()
})
