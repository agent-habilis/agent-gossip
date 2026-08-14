/**
 * The property table. One entry per supported CSS property, mapping it to a
 * *kind*.
 *
 * This is the single source for three things that would otherwise drift apart:
 * the TypeScript type of a property's value (`values.ts` derives `Declarations`
 * from this map), whether a bare number gets a `px` suffix when serialised
 * (`compile.ts` asks for the kind), and — should typed custom properties ever be
 * registered — the `@property` syntax. Adding a property is one line here and
 * nothing else.
 *
 * The table is curated rather than exhaustive. Roughly 120 properties covers
 * what the examples actually write; everything else, including anything shipped
 * after this file, goes through `raw()` at the value site, which costs one call
 * and keeps the surrounding declarations checked. See `values.ts`.
 *
 * `raw` as a *kind* means something different from the `raw()` escape: the
 * property is known and supported, but its value grammar is a shorthand or a
 * free-form list too irregular to be worth typing. `transform` and `background`
 * are the archetypes.
 */

export type Kind =
  | 'color'
  | 'length'
  | 'size'
  | 'margin'
  | 'number'
  | 'integer'
  | 'time'
  | 'angle'
  | 'display'
  | 'position'
  | 'flexDirection'
  | 'flexWrap'
  | 'align'
  | 'justify'
  | 'overflow'
  | 'textAlign'
  | 'fontWeight'
  | 'boxSizing'
  | 'whiteSpace'
  | 'cursor'
  | 'lineStyle'
  | 'objectFit'
  | 'textTransform'
  | 'visibility'
  | 'pointerEvents'
  | 'userSelect'
  | 'raw'

export const PROPS = {
  // ---- box ----------------------------------------------------------------
  width: 'size',
  height: 'size',
  minWidth: 'size',
  minHeight: 'size',
  maxWidth: 'size',
  maxHeight: 'size',
  inlineSize: 'size',
  blockSize: 'size',
  padding: 'length',
  paddingTop: 'length',
  paddingRight: 'length',
  paddingBottom: 'length',
  paddingLeft: 'length',
  paddingInline: 'length',
  paddingBlock: 'length',
  margin: 'margin',
  marginTop: 'margin',
  marginRight: 'margin',
  marginBottom: 'margin',
  marginLeft: 'margin',
  marginInline: 'margin',
  marginBlock: 'margin',
  boxSizing: 'boxSizing',
  aspectRatio: 'raw',

  // ---- layout -------------------------------------------------------------
  display: 'display',
  position: 'position',
  top: 'margin',
  right: 'margin',
  bottom: 'margin',
  left: 'margin',
  inset: 'margin',
  zIndex: 'integer',
  float: 'raw',
  clear: 'raw',
  visibility: 'visibility',
  overflow: 'overflow',
  overflowX: 'overflow',
  overflowY: 'overflow',
  isolation: 'raw',

  // ---- flexbox ------------------------------------------------------------
  flex: 'raw',
  flexDirection: 'flexDirection',
  flexWrap: 'flexWrap',
  flexGrow: 'number',
  flexShrink: 'number',
  flexBasis: 'size',
  alignItems: 'align',
  alignSelf: 'align',
  alignContent: 'justify',
  justifyContent: 'justify',
  justifyItems: 'align',
  justifySelf: 'align',
  gap: 'length',
  rowGap: 'length',
  columnGap: 'length',
  order: 'integer',

  // ---- grid ---------------------------------------------------------------
  grid: 'raw',
  gridTemplate: 'raw',
  gridTemplateColumns: 'raw',
  gridTemplateRows: 'raw',
  gridTemplateAreas: 'raw',
  gridAutoColumns: 'raw',
  gridAutoRows: 'raw',
  gridAutoFlow: 'raw',
  gridArea: 'raw',
  gridColumn: 'raw',
  gridRow: 'raw',
  placeItems: 'raw',
  placeContent: 'raw',
  placeSelf: 'raw',

  // ---- typography ---------------------------------------------------------
  color: 'color',
  font: 'raw',
  fontFamily: 'raw',
  fontSize: 'length',
  fontWeight: 'fontWeight',
  fontStyle: 'raw',
  fontVariant: 'raw',
  fontFeatureSettings: 'raw',
  fontVariantNumeric: 'raw',
  lineHeight: 'number',
  letterSpacing: 'length',
  wordSpacing: 'length',
  textAlign: 'textAlign',
  textTransform: 'textTransform',
  textDecoration: 'raw',
  textDecorationLine: 'raw',
  textDecorationColor: 'color',
  textDecorationThickness: 'length',
  textUnderlineOffset: 'length',
  textOverflow: 'raw',
  textIndent: 'length',
  textWrap: 'raw',
  whiteSpace: 'whiteSpace',
  wordBreak: 'raw',
  overflowWrap: 'raw',
  hyphens: 'raw',
  tabSize: 'integer',
  verticalAlign: 'raw',
  writingMode: 'raw',
  direction: 'raw',

  // ---- colour and painting ------------------------------------------------
  background: 'raw',
  backgroundColor: 'color',
  backgroundImage: 'raw',
  backgroundPosition: 'raw',
  backgroundSize: 'raw',
  backgroundRepeat: 'raw',
  backgroundClip: 'raw',
  backgroundOrigin: 'raw',
  backgroundAttachment: 'raw',
  opacity: 'number',
  mixBlendMode: 'raw',
  boxShadow: 'raw',
  textShadow: 'raw',
  filter: 'raw',
  backdropFilter: 'raw',
  accentColor: 'color',
  caretColor: 'color',
  colorScheme: 'raw',
  objectFit: 'objectFit',
  objectPosition: 'raw',

  // ---- borders ------------------------------------------------------------
  border: 'raw',
  borderTop: 'raw',
  borderRight: 'raw',
  borderBottom: 'raw',
  borderLeft: 'raw',
  borderWidth: 'length',
  borderTopWidth: 'length',
  borderRightWidth: 'length',
  borderBottomWidth: 'length',
  borderLeftWidth: 'length',
  borderStyle: 'lineStyle',
  borderColor: 'color',
  borderTopColor: 'color',
  borderRightColor: 'color',
  borderBottomColor: 'color',
  borderLeftColor: 'color',
  borderRadius: 'length',
  borderTopLeftRadius: 'length',
  borderTopRightRadius: 'length',
  borderBottomRightRadius: 'length',
  borderBottomLeftRadius: 'length',
  outline: 'raw',
  outlineColor: 'color',
  outlineWidth: 'length',
  outlineStyle: 'lineStyle',
  outlineOffset: 'length',

  // ---- svg ----------------------------------------------------------------
  fill: 'color',
  stroke: 'color',
  strokeWidth: 'length',
  strokeLinecap: 'raw',
  strokeLinejoin: 'raw',
  strokeDasharray: 'raw',

  // ---- interaction --------------------------------------------------------
  cursor: 'cursor',
  pointerEvents: 'pointerEvents',
  userSelect: 'userSelect',
  touchAction: 'raw',
  resize: 'raw',
  appearance: 'raw',
  scrollBehavior: 'raw',
  overscrollBehavior: 'raw',
  scrollSnapType: 'raw',
  scrollSnapAlign: 'raw',
  scrollMargin: 'length',
  scrollPadding: 'length',

  // ---- motion -------------------------------------------------------------
  transition: 'raw',
  transitionProperty: 'raw',
  transitionDuration: 'time',
  transitionDelay: 'time',
  transitionTimingFunction: 'raw',
  animation: 'raw',
  animationName: 'raw',
  animationDuration: 'time',
  animationDelay: 'time',
  animationIterationCount: 'raw',
  animationDirection: 'raw',
  animationFillMode: 'raw',
  animationTimingFunction: 'raw',
  transform: 'raw',
  transformOrigin: 'raw',
  translate: 'raw',
  rotate: 'angle',
  scale: 'raw',
  willChange: 'raw',

  // ---- containment --------------------------------------------------------
  contain: 'raw',
  contentVisibility: 'raw',
  containerType: 'raw',
  containerName: 'raw',

  // ---- generated content --------------------------------------------------
  content: 'raw',
  listStyle: 'raw',
  listStyleType: 'raw',
  listStylePosition: 'raw',
} as const satisfies Record<string, Kind>

export type PropName = keyof typeof PROPS

/**
 * Whether a bare number is a length for this property.
 *
 * `padding: 8` means 8px; `opacity: 8` would be nonsense with a unit. The kinds
 * that take unitless numbers are the exception, so they are the ones listed.
 */
const UNITLESS: ReadonlySet<Kind> = new Set<Kind>(['number', 'integer'])

export function isUnitless(prop: string): boolean {
  const kind = (PROPS as Record<string, Kind | undefined>)[prop]
  return kind === undefined || UNITLESS.has(kind)
}
