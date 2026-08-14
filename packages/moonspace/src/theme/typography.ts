/**
 * Typography — such as it is.
 *
 * One family, one size. Hierarchy comes from color, weight, inversion and spacing.
 * There is no `fontSize` token anywhere in this system, by design.
 */

/**
 * TX-02 is Berkeley Mono (U.S. Graphics Company). It is a paid, proprietary typeface
 * with a non-transferrable license, so it is deliberately *not* bundled here — you
 * self-host it if you own it, and the stack falls through if you don't.
 *
 * Nothing breaks without it. Every dimension in the system is expressed in `ch`, which
 * the browser resolves from whichever font actually loads; only the absolute pixel
 * width of the grid changes. See `GlobalStyle.ts` for the commented `@font-face` block.
 */
export const fontFamily = [
  "'TX-02'",
  "'TX-02 Variable'",
  'ui-monospace',
  'SFMono-Regular',
  'Menlo',
  'Consolas',
  "'Liberation Mono'",
  'monospace',
].join(', ')

/**
 * Two weights. Regular for everything, bold for the rare case where inversion is too
 * loud — terminals give us bold, so we can too. Anything in between doesn't survive
 * the port.
 */
export const fontWeight = {
  regular: 400,
  bold: 700,
} as const

export type FontWeight = keyof typeof fontWeight

/**
 * OpenType features, applied only when TX-02 is actually present (harmless otherwise).
 * `calt` enables Berkeley Mono's contextual ligatures; `ss01` is its slashed zero,
 * which matters when a component is displaying hashes or IDs.
 */
export const fontFeatureSettings = "'calt' 1, 'ss01' 1"
