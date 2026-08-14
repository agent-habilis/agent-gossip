/**
 * Builds `crates/agent-gossip-wasm-client` for wasm32 and runs `wasm-bindgen`
 * over the result, leaving the glue + `.wasm` in `app/wasm/pkg/` where the app
 * imports them.
 *
 * Not wasm-pack: it wants to own the manifest and the output layout, and the
 * crate is already a standalone workspace with its own reasons for both.
 */
import { mkdir, rm } from 'node:fs/promises'

const here = (path: string) => new URL(`../${path}`, import.meta.url).pathname

const CRATE = here('../crates/agent-gossip-wasm-client')
const OUT = here('app/wasm/pkg')

async function tool(name: string, args: string[], cwd: string, env: Record<string, string> = {}) {
  const proc = Bun.spawn([name, ...args], {
    cwd,
    env: { ...Bun.env, ...env },
    stdout: 'inherit',
    stderr: 'inherit',
  })
  if ((await proc.exited) !== 0) {
    console.error(`\n${name} ${args.join(' ')} failed`)
    process.exit(1)
  }
}

// Apple's clang has no wasm backend, and `ring`'s C core needs one. Homebrew's
// LLVM is the usual way to get it; without this the build dies deep inside a
// dependency with a message that does not mention the compiler.
const BREW_LLVM = '/opt/homebrew/opt/llvm/bin'
const env: Record<string, string> = {}
if (await Bun.file(`${BREW_LLVM}/clang`).exists()) {
  env['CC_wasm32_unknown_unknown'] = `${BREW_LLVM}/clang`
  env['AR_wasm32_unknown_unknown'] = `${BREW_LLVM}/llvm-ar`
}

console.log('cargo build --release --target wasm32-unknown-unknown …')
await tool('cargo', ['build', '--release', '--target', 'wasm32-unknown-unknown'], CRATE, env)

await rm(OUT, { recursive: true, force: true })
await mkdir(OUT, { recursive: true })

console.log('wasm-bindgen …')
await tool(
  'wasm-bindgen',
  [
    '--target',
    'web',
    '--out-dir',
    OUT,
    `${CRATE}/target/wasm32-unknown-unknown/release/agent_gossip_wasm_client.wasm`,
  ],
  CRATE,
  env,
)

const wasm = Bun.file(`${OUT}/agent_gossip_wasm_client_bg.wasm`)
console.log(`built ${(wasm.size / 1024 / 1024).toFixed(1)} MB of wasm into app/wasm/pkg/`)
