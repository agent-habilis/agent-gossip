import { Theme, configure, tokens } from 'visage-style'
import { grid } from 'moonspace'
import { COLOR_ROLES, superstylinDark, superstylinLight } from 'moonspace-theme'
import type { ColorRole, ColorTheme } from 'moonspace-theme'
import type { ElementNode } from 'visage-dom/types'

/*
 * Component rules go in a `moonspace` layer, and this call lives here rather
 * than in `index.ts` deliberately.
 *
 * `configure()` has to run before the first `Style()` — compiled text is cached
 * per style object, so anything already compiled keeps the settings it compiled
 * under. Putting it in the package entry point would be silently skipped by any
 * deep import of a single component module. Every component imports `t` or a
 * mixin, and every mixin imports `t`, so this module is the one place the call
 * cannot be bypassed.
 *
 * `GlobalStyle` declares `@layer reset, moonspace;` so the two have a defined
 * order rather than an accidental one.
 */
configure({ layer: 'moonspace' })

/**
 * The theme, as CSS custom properties.
 *
 * This replaces styled-components' `ThemeProvider` plus the `DefaultTheme`
 * augmentation, and it is a straight upgrade on both. The augmentation bought
 * one thing — a typo like `theme.color.acccent` failing to compile rather than
 * rendering as no colour — and `tokens()` keeps that while adding the piece it
 * could not express: a token carries its *kind*. `theme.color.accent` was a
 * `string` and would land anywhere a string went; `t.accent` is `Token<'color'>`
 * and is rejected in a length slot.
 *
 * There is no provider element and no context, because visage has neither. The
 * values live on `:root` and the cascade distributes them, which is also why
 * they survive into plain CSS and devtools.
 *
 * Declared against the default dark theme, but the values here are only
 * defaults: `MoonspaceTheme()` overrides them, which is what makes the palette
 * swappable. What `t` fixes for good is the *names* — every component compiles
 * against `var(--accent)` whatever colour ends up behind it.
 */
export const t = tokens({
  // --bg --bg-sunken --bg-raised --bg-selected --fg --fg-muted --fg-subtle
  // --fg-on-accent --border --border-strong --accent --accent-alt --success
  // --warning --danger --info
  ...superstylinDark.color,

  /*
   * The grid, spelled as literals rather than derived from `grid`.
   *
   * `grid.rowPx + 'px'` widens to `string`, `KindOf<string>` falls through to
   * `'raw'`, and `height: t.msRow` then fails as `Token<'raw'>` is not a Size.
   * The literal is what keeps the kind. `tokens.test.ts` asserts these against
   * `grid` so the duplication cannot drift silently.
   *
   * Note this is a real constraint for lengths and *not* one for colours: a
   * colour only has to be assignable to `Color`, and `ColorValue` is a union of
   * template literals that already is. So the palette above needs no `as const`
   * to infer `Token<'color'>`, while `'18px'` here genuinely does.
   */
  msFontSize: '15px',
  // Vendored grid fork: 22.5px row (1.5 line-height), not upstream's 18px.
  msRow: '22.5px',
  msMeasure: '80ch',
  msBorderWidth: '1px',
})

/**
 * Both palettes in one token set.
 *
 * `light-dark()` takes the two values and lets the browser pick per the
 * element's `color-scheme`, which `GlobalStyle` sets. So there is one block of
 * custom properties rather than a `:root` block, a `prefers-color-scheme` media
 * query and two `[data-theme]` overrides — and, more to the point, no way for
 * those four to drift out of agreement.
 *
 * Going through `color-scheme` rather than a media query is also what makes form
 * controls, scrollbars and the UA's own defaults follow the theme. A media query
 * would have switched our custom properties and left the browser's own rendering
 * on the opposite appearance.
 */
type Paired = Record<ColorRole, `light-dark(${string})`>

function pair(light: ColorTheme, dark: ColorTheme): Paired {
  const paired = {} as Paired
  for (const role of COLOR_ROLES) {
    paired[role] = `light-dark(${light.color[role]}, ${dark.color[role]})`
  }
  return paired
}

export interface MoonspaceThemeProps {
  /** Used when the resolved colour scheme is dark. Defaults to `superstylinDark`. */
  dark?: ColorTheme
  /** Used when the resolved colour scheme is light. Defaults to `superstylinLight`. */
  light?: ColorTheme
  /**
   * Where the custom properties land. `:root` unless you are theming a subtree.
   *
   * Custom properties inherit, so a second call with a selector re-themes
   * everything under it and nothing outside — which is the only way to put two
   * palettes on one page. Without it a second call would also write `:root` and
   * simply overwrite the first, silently re-colouring the whole document.
   */
  selector?: string
}

/**
 * The `<style>` element that gives the tokens their values. Render once, at the root.
 *
 * Pass a pair to swap the palette wholesale:
 *
 *     MoonspaceTheme({ dark: myDark, light: myLight })
 *
 * Or a selector, to theme one subtree while the rest of the page keeps the
 * default:
 *
 *     MoonspaceTheme({ dark: myDark, selector: '[data-panel]' })
 *
 * Only the roles in `COLOR_ROLES` are read, so a theme that satisfies
 * `ColorTheme` is guaranteed to be complete — there is no half-applied palette.
 */
export function MoonspaceTheme(props: MoonspaceThemeProps = {}): ElementNode<'style'> {
  const dark = props.dark ?? superstylinDark
  const light = props.light ?? superstylinLight
  const values = pair(light, dark)
  return props.selector === undefined
    ? Theme(t, { values })
    : Theme(t, { values, selector: props.selector })
}

/** The grid, as it is written in CSS. Exported so `tokens.test.ts` can check the drift. */
export const gridLiterals = {
  msFontSize: `${grid.fontSizePx}px`,
  msRow: `${grid.rowPx}px`,
  msMeasure: `${grid.measureCh}ch`,
  msBorderWidth: `${grid.borderPx}px`,
} as const
