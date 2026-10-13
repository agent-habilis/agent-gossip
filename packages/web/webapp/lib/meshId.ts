/**
 * Mesh ids, as `habilis-network-protocol` encodes them: base58check over
 *
 *   [1] version=2 [32] seed [1] name len [N] name [2] config len LE [..] config
 *
 * with a four-byte `SHA256(SHA256(payload))` tail. Bitcoin alphabet, no
 * prefix — a prefixed id is rejected engine-side too. The config starts with
 * the lookup flags (and a relay ladder when they say so), then one byte for the
 * transport policy: udp 0x01, webrtc 0x02, multihop 0x04, relay 0x08, gossip
 * 0x10. A bit above those is a build this decoder does not know, and the engine
 * refuses it with a request to upgrade.
 *
 * This is deliberately shared by the server's routing and the app's join form.
 * The server serves the app for any single path segment that validates, so a
 * looser test in one of the two would mean a link that routes but will not
 * connect, or the reverse.
 *
 * Validation is a real checksum, never a character-class regex: `/about` and a
 * one-character typo in a real id both have to 404 rather than open a room that
 * can never join anything.
 */

/**
 * The vector pinned in habilis-network's own suite (`habilis-network-protocol`
 * `src/mesh/tests.rs`, seed `[7u8; 32]`, name "test", public preset). Exported
 * so the unit test and the e2e suite pin the same value — two copies means an
 * engine change updates one and leaves the other testing a stale vector while
 * still looking green.
 */
export const GOLDEN_MESH_ID =
  'DqrLWcbLaiVzMV2mvqefxWxWmgLCYmGWAK5jCxeTgFqc6dk1MQrfiqazmpTSgH'

const VERSION = 2
const SEED_BYTES = 32
const KNOWN_TRANSPORT_BITS = 0x1f
const LOOKUP_RELAY = 0b0100
const LOOKUP_CUSTOM_RELAY = 0b1000

const ALPHABET = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'

const INDEX: ReadonlyMap<string, number> = new Map(
  [...ALPHABET].map((char, position) => [char, position]),
)

/** Smallest possible payload (empty name, empty config) plus the checksum. */
const MIN_BYTES = 1 + SEED_BYTES + 1 + 2 + 4

export function decodeBase58(text: string): Uint8Array<ArrayBuffer> | null {
  if (text.length === 0) return null

  // Little-endian scratch accumulator; reversed to big-endian at the end.
  const bytes: number[] = []
  for (const char of text) {
    const value = INDEX.get(char)
    if (value === undefined) return null

    let carry = value
    for (let index = 0; index < bytes.length; index += 1) {
      carry += (bytes[index] ?? 0) * 58
      bytes[index] = carry & 0xff
      carry >>= 8
    }
    while (carry > 0) {
      bytes.push(carry & 0xff)
      carry >>= 8
    }
  }

  // Positional arithmetic cannot represent a leading zero byte, so each leading
  // '1' carries one and they are reattached by hand. Any zero the accumulator
  // did produce at the top is dropped first — otherwise an all-'1' string, whose
  // value is zero, ends up one byte longer than it should be.
  while (bytes.length > 0 && bytes.at(-1) === 0) bytes.pop()
  for (const char of text) {
    if (char !== '1') break
    bytes.push(0)
  }

  return new Uint8Array(bytes.reverse())
}

// `Uint8Array<ArrayBuffer>`, not the default `ArrayBufferLike`: `digest` will
// not take a view that might sit on a SharedArrayBuffer, and every array here
// is built locally.
async function sha256(input: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
  // WebCrypto rather than Bun.CryptoHasher: this module runs in both the server
  // and the browser, and `crypto.subtle` is the only digest both of them have.
  // It takes a BufferSource directly, so there is nothing to copy.
  return new Uint8Array(await crypto.subtle.digest('SHA-256', input))
}

/** The bytes after the config length, or null when the lengths do not add up. */
function configRegion(payload: Uint8Array): Uint8Array | null {
  let position = 1 + SEED_BYTES
  const nameLength = payload[position]
  if (!nameLength) return null
  position += 1 + nameLength

  const low = payload[position]
  const high = payload[position + 1]
  if (low === undefined || high === undefined) return null
  const configLength = low | (high << 8)
  position += 2

  // The engine refuses trailing bytes, so the region has to end the payload.
  if (position + configLength !== payload.length) return null
  return payload.subarray(position)
}

/** The transport policy byte of a config region, or null when it is missing. */
function transportByte(config: Uint8Array): number | null {
  const flags = config[0]
  if (flags === undefined) return null
  let position = 1

  if (flags & LOOKUP_CUSTOM_RELAY) {
    if (!(flags & LOOKUP_RELAY)) return null
    const rungs = config[position]
    if (!rungs) return null
    position += 1
    for (let rung = 0; rung < rungs; rung += 1) {
      const low = config[position]
      const high = config[position + 1]
      if (low === undefined || high === undefined) return null
      position += 2 + (low | (high << 8))
    }
  }
  return config[position] ?? null
}

export async function isMeshId(candidate: string): Promise<boolean> {
  const raw = decodeBase58(candidate)
  if (!raw || raw.length < MIN_BYTES) return false

  const payload = raw.subarray(0, raw.length - 4)
  const checksum = raw.subarray(raw.length - 4)
  const expected = await sha256(await sha256(payload))

  for (let index = 0; index < 4; index += 1) {
    if (checksum[index] !== expected[index]) return false
  }
  // A future version would need its own parser, so an unknown one is refused
  // here just as the engine refuses it.
  if (payload[0] !== VERSION) return false

  const config = configRegion(payload)
  if (!config) return false
  const transport = transportByte(config)
  return transport !== null && (transport & ~KNOWN_TRANSPORT_BITS) === 0
}

/**
 * What someone actually has on the clipboard: a bare id, or the whole URL the
 * other tab's copy button produced. Returns the id, or null if neither.
 */
export async function parseMeshInput(input: string): Promise<string | null> {
  const trimmed = input.trim()
  if (trimmed === '') return null

  const candidates = [trimmed]

  // A pasted URL — `?mesh=` is where the copy button puts the id. The old
  // `/<id>` shape is deliberately not read: those links point at a route that
  // no longer exists, so accepting one would join a gossip from a URL that
  // 404s for whoever it is pasted to next.
  try {
    const query = new URL(trimmed).searchParams.get('mesh')
    if (query) candidates.push(query)
  } catch {
    // Not a URL. The bare-id candidate above still stands.
  }

  for (const candidate of candidates) {
    if (await isMeshId(candidate)) return candidate
  }
  return null
}
