import { readFile } from 'node:fs/promises'
import { join } from 'node:path'

// Read from the crate rather than typed here, so the chip cannot drift from
// the binary's version. Same trick as packages/scripts/build.ts. A cwd path
// rather than import.meta.url: Turbopack turns `new URL(…, import.meta.url)`
// into an asset import and refuses a file outside the package.
export async function crateVersion(): Promise<string> {
  const manifest = await readFile(join(process.cwd(), '../../crates/agent-gossip/Cargo.toml'), 'utf8')
  return /^version\s*=\s*"([^"]+)"/m.exec(manifest)?.[1] ?? '0.0.0'
}
