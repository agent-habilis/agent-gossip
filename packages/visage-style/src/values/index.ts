/**
 * The value grammar, and the style object type derived from it.
 *
 * Everything here is hand-rolled template-literal types — no `csstype`, no
 * dependency, and nothing that needs a build step. The trade is coverage: a
 * curated grammar catches the mistakes people actually make (`opacity: 'red'`,
 * `padding: '8'`) without trying to encode all of CSS in the type system, which
 * would cost `tsc` far more than it returns.
 *
 * `Declarations` is *derived* from `PROPS`, never hand-maintained, so a property
 * cannot exist in one and not the other.
 */

import type { Kind, PROPS } from '../props/index.ts'

// ---------------------------------------------------------------------------
// Primitives
// ---------------------------------------------------------------------------

/** Valid on every property, so every kind includes it. */
type Global = 'inherit' | 'initial' | 'unset' | 'revert' | 'revert-layer'

/**
 * The math functions stay universal: their result type depends on arguments we
 * are not going to parse, and rejecting `calc(…)` would reject valid CSS.
 *
 * `var(--x)` is deliberately *not* here. Admitting it everywhere would make a
 * custom property assignable to every kind, which is precisely the hole tokens
 * exist to close — a colour could land in a length slot and nothing would say
 * so. Reference custom properties through `tokens()`, which carries the kind, or
 * through `raw()` when you mean to opt out.
 */
type Fn =
  | `calc(${string})`
  | `min(${string})`
  | `max(${string})`
  | `clamp(${string})`
  | `anchor-size(${string})`

type Dynamic = Global | Fn

declare const TOKEN: unique symbol

/**
 * A design token: `var(--name)` at runtime, its declared kind to the checker.
 *
 * The brand is what makes a token narrower than the `var(--${string})` it
 * replaces. `Token<'length'>` is a member of `Length` and of nothing else, so
 * the type system can finally tell a spacing scale from a palette.
 */
export type Token<K extends string> = string & { readonly [TOKEN]: K }

type LengthUnit =
  | 'px' | 'rem' | 'em' | 'ex' | 'ch' | 'cap' | 'ic' | 'lh' | 'rlh'
  | '%' | 'vw' | 'vh' | 'vmin' | 'vmax' | 'vi' | 'vb'
  | 'svw' | 'svh' | 'lvw' | 'lvh' | 'dvw' | 'dvh'
  | 'cqw' | 'cqh' | 'cqi' | 'cqb' | 'cqmin' | 'cqmax'
  | 'pt' | 'pc' | 'cm' | 'mm' | 'in' | 'Q'

/** A bare number is a length: `padding: 8` is 8px, and `0` needs no unit. */
export type Length = number | `${number}${LengthUnit}` | Dynamic | Token<'length'>

/** Sizing properties take the intrinsic keywords that `padding` does not. */
export type Size =
  | Length
  | 'auto'
  | 'min-content'
  | 'max-content'
  | 'stretch'
  | `fit-content(${string})`
  | 'fit-content'

/** `margin` and the inset properties take `auto`, but not the intrinsic sizes. */
export type MarginValue = Length | 'auto'

export type Time = `${number}s` | `${number}ms` | Dynamic | Token<'time'>
export type Angle =
  | `${number}deg` | `${number}turn` | `${number}rad` | `${number}grad`
  | Dynamic | Token<'angle'>

type NamedColor =
  | 'transparent' | 'currentColor'
  | 'black' | 'white' | 'red' | 'green' | 'blue' | 'yellow' | 'orange' | 'purple'
  | 'gray' | 'grey' | 'silver' | 'maroon' | 'olive' | 'lime' | 'aqua' | 'teal'
  | 'navy' | 'fuchsia' | 'pink' | 'brown' | 'gold' | 'coral' | 'crimson'
  | 'indigo' | 'ivory' | 'khaki' | 'lavender' | 'salmon' | 'tan' | 'tomato'
  | 'violet' | 'wheat' | 'beige' | 'cyan' | 'magenta'
  // System colours. Worth having: they are how you follow the user's own theme
  // without shipping a palette, which is the whole point of `color-scheme`.
  | 'AccentColor' | 'AccentColorText' | 'ActiveText' | 'ButtonBorder'
  | 'ButtonFace' | 'ButtonText' | 'Canvas' | 'CanvasText' | 'Field' | 'FieldText'
  | 'GrayText' | 'Highlight' | 'HighlightText' | 'LinkText' | 'Mark' | 'MarkText'
  | 'SelectedItem' | 'SelectedItemText' | 'VisitedText'

export type Color =
  | NamedColor
  | `#${string}`
  | `rgb(${string})` | `rgba(${string})`
  | `hsl(${string})` | `hsla(${string})`
  | `hwb(${string})`
  | `lab(${string})` | `lch(${string})`
  | `oklab(${string})` | `oklch(${string})`
  | `color(${string})`
  | `color-mix(${string})`
  | `light-dark(${string})`
  | Dynamic | Token<'color'>

// ---------------------------------------------------------------------------
// Enumerated kinds
// ---------------------------------------------------------------------------

type Display =
  | 'block' | 'inline' | 'inline-block' | 'flex' | 'inline-flex' | 'grid'
  | 'inline-grid' | 'flow-root' | 'contents' | 'none' | 'table' | 'table-row'
  | 'table-cell' | 'list-item' | Dynamic

type Position = 'static' | 'relative' | 'absolute' | 'fixed' | 'sticky' | Dynamic

type Align =
  | 'stretch' | 'center' | 'start' | 'end' | 'flex-start' | 'flex-end'
  | 'baseline' | 'first baseline' | 'last baseline' | 'self-start' | 'self-end'
  | 'auto' | 'normal' | Dynamic

type Justify =
  | 'center' | 'start' | 'end' | 'flex-start' | 'flex-end' | 'left' | 'right'
  | 'space-between' | 'space-around' | 'space-evenly' | 'stretch' | 'normal'
  | Dynamic

/**
 * The kind→type map. One entry per `Kind`, and the reason the table in
 * `props.ts` can stay a list of one-liners.
 */
export interface KindType {
  color: Color
  length: Length
  size: Size
  margin: MarginValue
  number: number | Dynamic | Token<'number'>
  integer: number | Dynamic | Token<'number'>
  time: Time
  angle: Angle
  display: Display
  position: Position
  flexDirection: 'row' | 'row-reverse' | 'column' | 'column-reverse' | Dynamic
  flexWrap: 'nowrap' | 'wrap' | 'wrap-reverse' | Dynamic
  align: Align
  justify: Justify
  overflow: 'visible' | 'hidden' | 'clip' | 'scroll' | 'auto' | Dynamic
  textAlign: 'left' | 'right' | 'center' | 'justify' | 'start' | 'end' | Dynamic
  fontWeight:
    | 100 | 200 | 300 | 400 | 500 | 600 | 700 | 800 | 900
    | 'normal' | 'bold' | 'lighter' | 'bolder' | Dynamic
  boxSizing: 'content-box' | 'border-box' | Dynamic
  whiteSpace: 'normal' | 'nowrap' | 'pre' | 'pre-wrap' | 'pre-line' | 'break-spaces' | Dynamic
  cursor:
    | 'auto' | 'default' | 'none' | 'pointer' | 'progress' | 'wait' | 'text'
    | 'move' | 'help' | 'not-allowed' | 'grab' | 'grabbing' | 'crosshair'
    | 'col-resize' | 'row-resize' | 'ew-resize' | 'ns-resize' | 'zoom-in'
    | 'zoom-out' | Dynamic
  lineStyle:
    | 'none' | 'hidden' | 'solid' | 'dashed' | 'dotted' | 'double' | 'groove'
    | 'ridge' | 'inset' | 'outset' | Dynamic
  objectFit: 'fill' | 'contain' | 'cover' | 'none' | 'scale-down' | Dynamic
  textTransform: 'none' | 'capitalize' | 'uppercase' | 'lowercase' | Dynamic
  visibility: 'visible' | 'hidden' | 'collapse' | Dynamic
  pointerEvents: 'auto' | 'none' | Dynamic
  userSelect: 'auto' | 'none' | 'text' | 'all' | 'contain' | Dynamic
  /** Shorthands and free-form lists, deliberately untyped past `string`. */
  raw: string | number
}

/** Derived from `PROPS`. Never written by hand. */
export type Declarations = {
  readonly [K in keyof typeof PROPS]?: KindType[(typeof PROPS)[K] & Kind]
}

// ---------------------------------------------------------------------------
// The escape hatch
// ---------------------------------------------------------------------------

declare const RAW: unique symbol

/** A string that has opted out of value checking. Still scoped, still validated in DEV. */
export type Raw = string & { readonly [RAW]: true }

/**
 * Opt one declaration out of the type system.
 *
 *     css({ padding: '8px', anchorName: raw('--tip') })
 *
 * The signature rejects a non-literal argument, and that is not pedantry.
 * Compiled CSS is cached on the identity of the style object, so a value that
 * varied per call would quietly produce a rule per value — the precise failure
 * mode of runtime CSS-in-JS that scoped static rules exist to avoid. A literal
 * cannot vary, so the cache cannot be defeated by accident.
 *
 * Escaping the *type system* does not escape the *scoping*: raw text compiles
 * into the same `@scope` block and the same layer as everything else, and DEV
 * still runs it past `CSS.supports`.
 */
export function raw<const S extends string>(value: string extends S ? never : S): Raw {
  return value as unknown as Raw
}

// ---------------------------------------------------------------------------
// The style object
// ---------------------------------------------------------------------------

/**
 * What `Style()` accepts before checking.
 *
 * Two jobs, and they pull in opposite directions. The `Declarations` members are
 * what give autocomplete — they are why typing `pad` offers `padding`. The index
 * signature is what admits unknown keys at all, which `raw()` needs to reach an
 * unlisted property and nested blocks need to exist. Its value type is written
 * out rather than left as `unknown` so that a nested block is contextually a
 * `StyleInput` too, and autocomplete survives one level down.
 *
 * On its own this would let `paddng: '8px'` through. `CheckStyle` is what
 * catches it.
 */
export interface StyleInput extends Escapable {
  readonly [key: string]: StyleInput | Raw | string | number | undefined
}

/**
 * `Declarations`, but every property will also take a `raw()`.
 *
 * A known property whose value the grammar cannot express is the common case
 * this exists for — `padding: raw('3px 8px')`, where the real syntax is one to
 * four lengths and `Length` is one. Without this, `raw()` could only reach
 * properties the table has never heard of, which is the half of the problem it
 * was *not* introduced to solve.
 *
 * It costs no strictness: `Raw` is branded, so only a value that came out of
 * `raw()` satisfies it, and a bare string still has to match the grammar.
 */
type Escapable = {
  readonly [K in keyof Declarations]?: Declarations[K] | Raw
}

/** The message a bad key reports. A tuple, so the text lands in the error. */
type UnknownProperty = readonly [
  'unknown CSS property — wrap the value in raw() to opt out of checking',
]

/**
 * The half of the check that an index signature cannot do.
 *
 * Runs over the *literal* type of the argument and maps every key to the type
 * its value must satisfy. Intersecting the result back with the argument is what
 * enforces it, and what makes a bad key unassignable.
 *
 * It re-states `Declarations[K]` rather than deferring to `StyleInput` because
 * nested blocks reach this function too — inside `'&:hover'` the index signature
 * has already widened everything to `StyleInput`, so this is the only thing left
 * checking that `opacity` is a number.
 *
 * `Raw` is tested first, and the order is not cosmetic: `Raw` is
 * `string & {…}`, so it satisfies `extends object` and would otherwise be
 * recursed into as though it were a nested block.
 *
 * Structural recursion only — no `infer`, no template-literal parsing — so it
 * costs `tsc` one mapped type per nesting level and nothing more.
 */
export type CheckStyle<T> = {
  readonly [K in keyof T]: T[K] extends Raw
    ? unknown
    : K extends keyof Declarations
      ? Declarations[K]
      : K extends `--${string}`
        ? string | number
        : T[K] extends object
          ? CheckStyle<T[K]>
          : UnknownProperty
}

/** A compiled style object as the runtime sees it, with all typing erased. */
export type StyleNode = {
  readonly [key: string]: string | number | StyleNode | undefined
}
