import type { Attrs } from 'visage-dom/types'

/**
 * Merge a caller's `style` prop with the custom properties a component sets.
 *
 * Components carry their per-instance values — a colour role, a column
 * template, a padding in cells — as CSS custom properties on the inline style,
 * because those are the values that cannot live in a hoisted stylesheet. A
 * caller passing `style` must not lose them, and vice versa.
 *
 * `style` may also arrive as a cssText string, which cannot be merged key-wise.
 * It is handed back under `cssText`, which `applyStyle` assigns wholesale, so
 * the caller's declarations still land.
 */
export function withStyle(
  style: Attrs<'span'>['style'],
  own: Record<string, string | number>,
): Record<string, string | number> {
  if (style === undefined) return own
  if (typeof style === 'string') return { ...own, cssText: style }
  return { ...own, ...(style as Record<string, string | number>) }
}

/**
 * `'true'` / `'false'` for a `dataset` entry, defaulting an absent prop to false.
 *
 * Only the default is doing real work — `applyDataset` already stringifies, so
 * `data-x="false"` would survive either way, and `[data-x="false"]` selectors
 * are safe. `undefined` is the case that needs handling: left alone it writes
 * the literal string `"undefined"`, which matches no selector the component
 * declares.
 *
 * Note this is *not* true of the `attrs` prop, which removes an attribute
 * outright on `false`. Boolean state belongs in `dataset`; ARIA state that must
 * read `"false"` has to be written as an explicit string.
 */
export const flag = (value: boolean | undefined): string => String(value ?? false)
