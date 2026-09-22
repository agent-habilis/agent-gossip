import { expect, test } from 'bun:test'

import { GOLDEN_MESH_ID, decodeBase58, isMeshId, parseMeshInput } from './meshId.ts'

// If this stops validating, this decoder and the engine have diverged and every
// link the site produces is suspect.
const GOLDEN = GOLDEN_MESH_ID

test('the engine golden vector validates', async () => {
  expect(await isMeshId(GOLDEN)).toBe(true)
})

test('a one-character change fails the checksum', async () => {
  // The whole reason this is a checksum and not a regex: a typo has to 404
  // rather than open a room that can never connect.
  const swapped = `${GOLDEN.slice(0, -1)}${GOLDEN.endsWith('1') ? '2' : '1'}`
  expect(await isMeshId(swapped)).toBe(false)
})

test('reserved-looking paths cannot collide with an id', async () => {
  for (const path of ['about', 'docs', 'mesh', 'new', 'index.html', 'style.css']) {
    expect(await isMeshId(path)).toBe(false)
  }
})

test('non-base58 characters are rejected', async () => {
  // 0, O, I and l are excluded from the alphabet precisely so they cannot be
  // confused for one another when read aloud or retyped.
  for (const bad of ['0OIl', `${GOLDEN.slice(0, -1)}0`, 'not a hash', '']) {
    expect(await isMeshId(bad)).toBe(false)
  }
})

test('a pasted URL yields the id', async () => {
  for (const input of [
    // The shape the app hands out. The id is in the query, so a parser that
    // reads the last path segment sees `app` and refuses the very link the
    // copy button produced.
    `https://agent-gossip.com/app/?mesh=${GOLDEN}`,
    `https://agent-gossip.localhost/app/?mesh=${GOLDEN}&nickname=tab`,
    `  ${GOLDEN}  `,
  ]) {
    expect(await parseMeshInput(input)).toBe(GOLDEN)
  }
})

test('a URL with no mesh parameter yields null', async () => {
  expect(await parseMeshInput('https://agent-gossip.com/about')).toBe(null)
  expect(await parseMeshInput('')).toBe(null)
  // The pre-`?mesh=` link shape. Refused on purpose: `/<id>` is not a route any
  // more, so a link like this cannot be shared onward even if it parsed.
  expect(await parseMeshInput(`https://agent-gossip.com/${GOLDEN}`)).toBe(null)
})

test('leading ones decode to leading zero bytes', async () => {
  // Base58 cannot represent a leading zero byte positionally, so they are
  // reattached by hand; getting that wrong shortens every id starting with '1'.
  expect([...(decodeBase58('11') ?? [])]).toEqual([0, 0])
  expect([...(decodeBase58('1z') ?? [])]).toEqual([0, 57])
})
