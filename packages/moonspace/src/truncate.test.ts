import { expect, test } from 'bun:test'
import { middleTruncate } from './truncate.ts'
import { glyphs } from './theme/glyphs.ts'

const E = glyphs.ellipsis

test('a string within budget is returned untouched', () => {
  expect(middleTruncate('deploy', 6)).toBe('deploy')
  expect(middleTruncate('deploy', 10)).toBe('deploy')
})

test('the result never exceeds the budget', () => {
  const value = 'packages/moonspace/src/components/Table/Table.tsx'
  for (let budget = 1; budget <= value.length + 2; budget++) {
    expect([...middleTruncate(value, budget)].length).toBeLessThanOrEqual(budget)
  }
})

test('the head keeps the odd character, because leading context disambiguates', () => {
  // 'abcdefgh' at 5: keep 4, head 2, tail 2.
  expect(middleTruncate('abcdefgh', 5)).toBe(`ab${E}gh`)
  // At 6: keep 5, head 3, tail 2 — the extra character goes to the head.
  expect(middleTruncate('abcdefgh', 6)).toBe(`abc${E}gh`)
})

test('degenerate budgets', () => {
  expect(middleTruncate('deploy', 0)).toBe('')
  expect(middleTruncate('deploy', -1)).toBe('')
  expect(middleTruncate('deploy', 1)).toBe(E)
  expect(middleTruncate('deploy', 2)).toBe(`d${E}`)
})

test('counts characters, not code units', () => {
  // Astral-plane characters are two code units each; a naive slice would split
  // one in half and emit a lone surrogate.
  const value = '🌑🌒🌓🌔🌕🌖🌗🌘'
  const out = middleTruncate(value, 5)
  expect([...out].length).toBe(5)
  expect(out).toBe(`🌑🌒${E}🌗🌘`)
})
