import { glyphs } from './theme/glyphs.ts'

/**
 * Splits a string so it fits `budget` characters, keeping the start and the end.
 *
 * Pure and synchronous, so it is trivially testable and reusable by either
 * renderer. That is why it lives here rather than beside the DOM component: a
 * terminal already knows its width in columns and can call this directly,
 * with none of the measurement machinery the browser needs.
 *
 * The head gets the odd character when the budget is even, because for the
 * strings this is built for — paths, URLs, branch names — the leading context is
 * what usually disambiguates.
 */
export function middleTruncate(value: string, budget: number): string {
  const chars = [...value]
  if (budget <= 0) return ''
  if (chars.length <= budget) return value
  if (budget === 1) return glyphs.ellipsis

  const keep = budget - 1
  const head = Math.ceil(keep / 2)
  const tail = keep - head

  return `${chars.slice(0, head).join('')}${glyphs.ellipsis}${tail > 0 ? chars.slice(-tail).join('') : ''}`
}
