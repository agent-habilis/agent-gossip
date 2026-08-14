import type { AnsiColor, ColorRole } from './roles.ts'

/**
 * Every semantic role, collapsed onto the ANSI-16 palette.
 *
 * This is the terminal's entire colour system. `moonspace-tui` reads this and
 * nothing else — it never sees a hex, and there is no terminal equivalent of the
 * `ColorTheme`s in `themes/`.
 *
 * That is deliberate. A terminal already has a palette, chosen by the person
 * using it and already tuned for their background; emitting our own truecolour
 * values over the top would be overriding a preference they have already
 * expressed. So the roles resolve to slot *names* and the values arrive from the
 * far end.
 *
 * Which is also why there is one map here and not one per appearance. `default`
 * means "the terminal's own foreground/background", so `bg`, `fg` and
 * `borderStrong` are correct in a light terminal and a dark one without
 * branching. Spelling `fg` as `white` — as this mapping used to — silently
 * assumed everyone was on a dark background.
 *
 * Note the collisions: `bgSunken`/`bgRaised` both land on `default`, and
 * `fgMuted`/`fgSubtle` both on `brightBlack`. That is precisely why depth and
 * state must never be communicated by colour alone — see the inverse-video and
 * glyph rules in the component docs. Since ANSI-16 is now the terminal's only
 * rendering path rather than a fallback behind a flag, that constraint is
 * enforced by construction: a component that cheats with hue is visibly broken
 * in the terminal, not merely theoretically fragile.
 */
export const ansi: Record<ColorRole, AnsiColor> = {
  bg: 'default',
  bgSunken: 'default',
  bgRaised: 'default',
  // Paired with inverse video, which is what actually carries a selection.
  bgSelected: 'blue',

  fg: 'default',
  fgMuted: 'brightBlack',
  fgSubtle: 'brightBlack',
  // An accent fill is drawn with `inverse: true`, which swaps the terminal's own
  // background in as the text colour.
  fgOnAccent: 'default',

  border: 'brightBlack',
  borderStrong: 'default',

  // Non-bright: the bright variants wash out against a light background, and
  // these have to survive both.
  accent: 'blue',
  accentAlt: 'magenta',

  success: 'green',
  warning: 'yellow',
  danger: 'red',
  info: 'cyan',
}
