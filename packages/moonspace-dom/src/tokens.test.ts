import { expect, test } from 'bun:test'
import { grid, gridVars } from 'moonspace'
import { COLOR_ROLES, superstylinDark, superstylinLight } from 'moonspace-theme'
import { GlobalStyle } from './GlobalStyle.ts'
import { MoonspaceTheme, gridLiterals, t } from './tokens.ts'

const themeCss = String(MoonspaceTheme().children[0])
const globalCss = String(GlobalStyle().children[0])

/** `msRow` → `--ms-row`, the rule `tokens()` applies to every key. */
const customProperty = (name: string): string =>
  `--${name.replace(/[A-Z]/g, (m) => `-${m.toLowerCase()}`)}`

test('every semantic role reaches :root as a light-dark() pair', () => {
  /*
   * Both palettes in one declaration, light first — that is the argument order
   * `light-dark()` defines, and getting it backwards would invert the whole
   * theme while still typechecking and still rendering colours.
   */
  for (const role of COLOR_ROLES) {
    const light = superstylinLight.color[role]
    const dark = superstylinDark.color[role]
    expect(themeCss).toContain(`${customProperty(role)}:light-dark(${light}, ${dark});`)
  }
})

test('a custom pair overrides both halves', () => {
  // The whole point of the package split: the names stay, the values move.
  const custom = {
    ...superstylinDark,
    name: 'custom',
    color: { ...superstylinDark.color, accent: '#ff0000' },
  } as const
  const css = String(MoonspaceTheme({ dark: custom }).children[0])
  expect(css).toContain(`--accent:light-dark(${superstylinLight.color.accent}, #ff0000);`)
  // Roles the custom theme did not change are untouched.
  expect(css).toContain(`--fg:light-dark(${superstylinLight.color.fg}, ${superstylinDark.color.fg});`)
})

test('the grid literals match the grid they were copied from', () => {
  /*
   * The one piece of duplication in the token map. `tokens()` infers a token's
   * kind from its declared value, and `grid.rowPx + 'px'` widens to `string`,
   * which falls through to `Token<'raw'>` and can no longer be a height. Writing
   * the literal is what keeps the kind — this test is what keeps the literal
   * honest.
   *
   * Colours need no equivalent: `ColorValue` is already a union of template
   * literals assignable to `Color`, so a role infers `Token<'color'>` without
   * being pinned to one hex. That asymmetry is why the palette is swappable and
   * the grid is not.
   */
  expect(gridLiterals).toEqual({
    msFontSize: '15px',
    // Vendored grid fork: 22.5px row, not upstream's 18px.
    msRow: '22.5px',
    msMeasure: '80ch',
    msBorderWidth: '1px',
  })
  for (const value of Object.values(gridLiterals)) {
    expect(themeCss).toContain(`:${value};`)
  }
  expect(gridLiterals.msRow).toBe(`${grid.rowPx}px`)
  expect(gridLiterals.msFontSize).toBe(`${grid.fontSizePx}px`)
  expect(gridLiterals.msMeasure).toBe(`${grid.measureCh}ch`)
  expect(gridLiterals.msBorderWidth).toBe(`${grid.borderPx}px`)
})

test('the emitted property names are the ones gridVars already declared', () => {
  // Components and hand-written CSS reference these names directly, so a rename
  // in either place has to be a rename in both.
  for (const name of Object.values(gridVars)) {
    expect(themeCss).toContain(`${name}:`)
  }
})

test('tokens read back as var() references', () => {
  // String() rather than a bare comparison: a token is a *branded* string
  // (`Token<'color'>`), and the brand is the whole point — it is what stops a
  // colour landing in a length slot. Asserting the runtime value means shedding
  // the brand deliberately here.
  expect(String(t.accent)).toBe('var(--accent)')
  expect(String(t.fgOnAccent)).toBe('var(--fg-on-accent)')
  expect(String(t.msRow)).toBe('var(--ms-row)')
})

test('the theme is unscoped, because inheritance is the point', () => {
  expect(themeCss).toContain(':root{')
  expect(themeCss).not.toContain('@scope')
})

test('the reset is unscoped and declares its layer order first', () => {
  // @scope would bind these rules to the <style> element's parent, which is the
  // opposite of what html/body/::selection need.
  expect(globalCss).not.toContain('@scope')
  expect(globalCss.indexOf('@layer reset, moonspace;')).toBeGreaterThanOrEqual(0)
  expect(globalCss.indexOf('@layer reset, moonspace;')).toBeLessThan(globalCss.indexOf('@layer reset {'))
})

test('color-scheme is declared, and can be forced both ways', () => {
  /*
   * Without this the light-dark() pairs would never resolve to their light half,
   * and — the part a media query could not have done — form controls, scrollbars
   * and the UA's own defaults would stay on one appearance while everything
   * else switched.
   */
  expect(globalCss).toContain('color-scheme: light dark;')
  expect(globalCss).toContain('[data-theme="light"] { color-scheme: light; }')
  expect(globalCss).toContain('[data-theme="dark"] { color-scheme: dark; }')

  // The forcing rules must come after :root — same specificity (0,1,0) on the
  // same element, so source order is the only thing deciding the winner.
  expect(globalCss.indexOf(':root { color-scheme: light dark; }')).toBeLessThan(
    globalCss.indexOf('[data-theme="light"]'),
  )
})

test('the color-scheme rules are unlayered', () => {
  /*
   * A regression test for a bug that was invisible in every unit assertion above.
   *
   * Inside `@layer reset` these three rules parse, match, and lose — unlayered
   * author CSS beats every layered rule, and an app that sets `color-scheme` on
   * `html` in static markup (as stories-dom does, to paint the first frame) has
   * exactly such a rule. The page kept its dark values with data-theme="light"
   * set and nothing anywhere to explain why.
   *
   * Position is the whole assertion: these have to appear before the layer block
   * opens.
   */
  const layerStart = globalCss.indexOf('@layer reset {')
  expect(layerStart).toBeGreaterThan(0)
  for (const rule of [
    ':root { color-scheme: light dark; }',
    '[data-theme="light"] { color-scheme: light; }',
    '[data-theme="dark"] { color-scheme: dark; }',
  ]) {
    expect(globalCss.indexOf(rule)).toBeGreaterThanOrEqual(0)
    expect(globalCss.indexOf(rule)).toBeLessThan(layerStart)
  }
})

test('a selector scopes the theme to a subtree instead of :root', () => {
  /*
   * Without this, two themes on one page is not "the second one wins in its
   * subtree" — it is "the second one silently rewrites :root and re-colours the
   * whole document", which looks like it works until you put the default next to
   * it for comparison.
   */
  const scoped = String(MoonspaceTheme({ selector: '[data-panel]' }).children[0])
  expect(scoped.startsWith('[data-panel]{')).toBe(true)
  expect(scoped).not.toContain(':root')
  // The default is still :root, so the common case needs no argument.
  expect(themeCss.startsWith(':root{')).toBe(true)
})

test('the reset reads colours through the token properties, not hexes', () => {
  expect(globalCss).toContain('background: var(--bg);')
  expect(globalCss).toContain('color: var(--fg);')
  // A hex here would mean the reset had forked from the theme.
  expect(globalCss).not.toContain(superstylinDark.color.bg)
})
