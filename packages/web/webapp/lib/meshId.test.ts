import { expect, test } from 'bun:test'

import { GOLDEN_MESH_ID, decodeBase58, isMeshId, parseMeshInput } from './meshId.ts'

// The id that habilis-network pins in its own suite (mesh id version 2; seed
// `[7u8; 32]`, name "test", public preset: udp, webrtc, multihop, gossip). If
// this stops validating, this decoder and the engine have diverged and every
// link the site produces is suspect.
const ENGINE_V2_ID = 'DqrLWcbLaiVzMV2mvqefxWxWmgLCYmGWAK5jCxeTgFqc6dk1MQrfiqazmpTSgH'
// The same mesh as an older default made it: no gossip bit. The engine still
// decodes it, so this decoder must too.
const OLDER_DEFAULT_ID = 'DqrLWcbLaiVzMV2mvqefxWxWmgLCYmGWAK5jCxeTgFqc6dk1MQrfiqay2YxkRe'
// The version 1 id of the previous engine for the same seed and name. Every id
// of that version is refused with a request to upgrade.
const V1_ID = '2UXAThUkdBAbiJNXvCt4YeMGQ9myFg7gJJZSr3pG3MAGzUwWmmV7D2NgrWBn1'

const GOLDEN = GOLDEN_MESH_ID

const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

function encodeBase58(bytes: Uint8Array): string {
  const digits: number[] = []
  for (const byte of bytes) {
    let carry = byte
    for (let index = 0; index < digits.length; index += 1) {
      carry += (digits[index] ?? 0) << 8
      digits[index] = carry % 58
      carry = Math.floor(carry / 58)
    }
    while (carry > 0) {
      digits.push(carry % 58)
      carry = Math.floor(carry / 58)
    }
  }
  let text = ''
  for (const byte of bytes) {
    if (byte !== 0) break
    text += '1'
  }
  for (const digit of digits.reverse()) text += ALPHABET[digit]
  return text
}

async function sha256(input: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
  return new Uint8Array(await crypto.subtle.digest('SHA-256', input))
}

/** Base58check of `payload`, with the double-SHA-256 tail the engine writes. */
async function seal(payload: Uint8Array<ArrayBuffer>): Promise<string> {
  const tail = (await sha256(await sha256(payload))).subarray(0, 4)
  const bytes = new Uint8Array(payload.length + 4)
  bytes.set(payload)
  bytes.set(tail, payload.length)
  return encodeBase58(bytes)
}

/** The engine's version 2 id with `config` as its config region. */
async function withConfig(config: number[]): Promise<string> {
  const raw = decodeBase58(ENGINE_V2_ID)
  if (!raw) throw new Error('the engine id does not decode')
  // version, seed, name length, name "test"
  const header = raw.subarray(0, 1 + 32 + 1 + 4)
  const payload = new Uint8Array(header.length + 2 + config.length)
  payload.set(header)
  payload.set([config.length & 0xff, config.length >> 8], header.length)
  payload.set(config, header.length + 2)
  return seal(payload)
}

test('the golden vector is the one the engine pins', () => {
  expect(GOLDEN).toBe(ENGINE_V2_ID)
})

test('the engine golden vector validates', async () => {
  expect(await isMeshId(GOLDEN)).toBe(true)
})

test('an id of an older default mesh, with no gossip bit, validates', async () => {
  expect(await isMeshId(OLDER_DEFAULT_ID)).toBe(true)
})

test('an id of version 1 is refused', async () => {
  expect(await isMeshId(V1_ID)).toBe(false)
})

test('every transport bit the engine knows is accepted', async () => {
  // udp 0x01, webrtc 0x02, multihop 0x04, relay 0x08, gossip 0x10
  for (const transport of [0x01, 0x02, 0x03, 0x07, 0x0b, 0x17, 0x1f]) {
    expect(await isMeshId(await withConfig([0x07, transport]))).toBe(true)
  }
})

test('a transport bit above 0x1f is refused', async () => {
  for (const transport of [0x20, 0x40, 0x80, 0x37, 0xff]) {
    expect(await isMeshId(await withConfig([0x07, transport]))).toBe(false)
  }
})

test('the transport byte is found after a custom relay ladder', async () => {
  // flags: mdns, dht, relay, custom ladder; one rung of 17 bytes
  const rung = [...new TextEncoder().encode('https://a.example')]
  const ladder = [0x0f, 0x01, rung.length, 0x00, ...rung]
  expect(await isMeshId(await withConfig([...ladder, 0x17]))).toBe(true)
  expect(await isMeshId(await withConfig([...ladder, 0x20]))).toBe(false)
})

test('a config with no transport byte is refused', async () => {
  expect(await isMeshId(await withConfig([0x07]))).toBe(false)
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
