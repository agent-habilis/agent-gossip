/**
 * The colour contract: what a role is called, and what shape its value takes.
 *
 * Nothing in this file names a colour. That separation is the point — the roles
 * are the vocabulary components are allowed to use, and a palette is one set of
 * answers to them. Swapping the answers is `themes/`; changing the questions is
 * a breaking change to every component in both renderers.
 */

/**
 * Every semantic role, as a runtime tuple.
 *
 * A tuple rather than `keyof typeof someTheme`, because a role list derived from
 * one theme would make that theme the definition of the contract — and the
 * conformance test in `theme.test.ts` needs a list that no single theme owns in
 * order to check them all against it.
 *
 * The set is deliberately small. A monospace system has no type scale to lean
 * on, so colour is doing more hierarchy work than usual; keeping the vocabulary
 * tight is what stops it sprawling into an arbitrary palette-by-another-name.
 */
export const COLOR_ROLES = [
  /** Page background. */
  'bg',
  /** Recessed surfaces: code blocks, table headers, inset wells. */
  'bgSunken',
  /** Raised surfaces: menus, popovers, hovered rows. */
  'bgRaised',
  /** Active selection. Pairs with `fg` — this is the "inverse video" background. */
  'bgSelected',

  /** Body text. */
  'fg',
  /** Secondary text: labels, captions, inactive tabs. */
  'fgMuted',
  /** Tertiary text: placeholders, disabled controls, the dim half of a progress bar. */
  'fgSubtle',
  /** Text drawn on top of an `accent` fill. */
  'fgOnAccent',

  /** Default rules and component chrome. */
  'border',
  /** Emphasized rules: focused fields, active panel edges. */
  'borderStrong',

  /** Primary interactive colour: focus, links, selected tab. */
  'accent',
  /** Secondary interactive colour: visited, alternate emphasis. */
  'accentAlt',

  /** Status. */
  'success',
  'warning',
  'danger',
  'info',
] as const

export type ColorRole = (typeof COLOR_ROLES)[number]

/**
 * The 16 slots every terminal has, whatever its colour depth, plus `default`.
 *
 * `default` is not a seventeenth colour — it is the absence of one, meaning "the
 * terminal's own foreground/background". It is the only value that is correct in
 * both a light and a dark terminal, which is why several roles resolve to it.
 */
export type AnsiColor =
  | 'black'
  | 'red'
  | 'green'
  | 'yellow'
  | 'blue'
  | 'magenta'
  | 'cyan'
  | 'white'
  | 'brightBlack'
  | 'brightRed'
  | 'brightGreen'
  | 'brightYellow'
  | 'brightBlue'
  | 'brightMagenta'
  | 'brightCyan'
  | 'brightWhite'
  | 'default'

/**
 * A CSS colour value.
 *
 * Deliberately a *subset* of `visage-style`'s `Color`, restated rather than
 * imported. Neither `moonspace-tui` nor `stories-tui` maps `visage-style` in its
 * tsconfig `paths`, and tsc typechecks workspace dependencies from source, so
 * the import would be an unresolved module in the terminal half of the repo.
 * Every member here is a member of visage's `Color`, which is all assignability
 * needs — a `Record<ColorRole, ColorValue>` passes straight into `Theme()`'s
 * `values` override.
 *
 * A bare `string` would be the obvious spelling and does not work: visage's
 * `Color` is a closed union of template-literal types with no `string` member,
 * so `Record<ColorRole, string>` fails with "Type 'string' is not assignable to
 * type 'Color'". The template literals are what make this assignable.
 *
 * Named colours and `inherit`/`currentColor` are left out on purpose: a theme
 * states a colour, it does not defer to one.
 */
export type ColorValue =
  | `#${string}`
  | `rgb(${string})`
  | `rgba(${string})`
  | `hsl(${string})`
  | `hsla(${string})`
  | `hwb(${string})`
  | `lab(${string})`
  | `lch(${string})`
  | `oklab(${string})`
  | `oklch(${string})`
  | `color(${string})`
  | `color-mix(${string})`
