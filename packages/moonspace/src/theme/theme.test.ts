import { expect, test } from 'bun:test'
import { grid, gridVars } from './grid.ts'

/*
 * What is left here is the geometry: the grid and the glyph vocabulary. The
 * colour invariants moved to `moonspace-theme` along with the colours
 * themselves, and now run over every built-in palette rather than the single
 * one this package used to hardcode.
 */

test('the row/font ratio stays at the vendored 1.5 fork', () => {
  // grid.ts: upstream keeps 1.2 for box-drawing joins; this vendor copy runs
  // 25% taller for the share browser's readability. 22.5px halves to a clean
  // 11.25 on the half-row, and every stacked row lands on .0 or .5.
  expect(grid.rowPx / grid.fontSizePx).toBe(1.5)
  expect(grid.rowPx).toBe(22.5)
})

test('the grid custom property names are the ones CSS writes by hand', () => {
  expect(gridVars).toEqual({
    fontSize: '--ms-font-size',
    row: '--ms-row',
    measure: '--ms-measure',
    border: '--ms-border-width',
  })
})
