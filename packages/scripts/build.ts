/**
 * Produces `dist/`, the served document root:
 *
 *   server/public/*   copied verbatim  -> server/dist/*      (the marketing site)
 *   app/index.html    bundled          -> server/dist/app/*  (the gossip app)
 *
 * Neither `app/` nor `server/public/` is served directly, which is what keeps
 * the app sources — and `server.ts`, `package.json`, `.env` — unreachable.
 *
 * Bun reads `jsx` / `jsxImportSource` out of tsconfig, so there is no JSX
 * configuration here.
 */
import { cp, rm } from 'node:fs/promises'

const here = (path: string) => new URL(`../${path}`, import.meta.url).pathname

const DIST = here('server/dist/')

await rm(DIST, { recursive: true, force: true })

/**
 * Read from the crate rather than duplicated here, so `gossip_version` in the
 * browser cannot drift from the binary's. A drift would be invisible: both
 * would answer, and only one would be right.
 */
async function crateVersion(): Promise<string> {
  const manifest = await Bun.file(here('../crates/agent-gossip/Cargo.toml')).text()
  return /^version\s*=\s*"([^"]+)"/m.exec(manifest)?.[1] ?? '0.0.0'
}

/**
 * The bundler cannot see the `.wasm`: the bindgen glue resolves it at runtime
 * with `new URL(..., import.meta.url)`, so it has to be copied in by hand and
 * the path handed to `init()` explicitly.
 *
 * Content-addressed, because at ~6.6 MB it is by far the largest asset on the
 * connect path and must be cacheable forever — while still changing name the
 * moment the crate does.
 */
async function stageWasm(): Promise<string> {
  const source = Bun.file(here('app/wasm/pkg/agent_gossip_wasm_client_bg.wasm'))
  if (!(await source.exists())) {
    console.error('missing app/wasm/pkg — run `bun run build:wasm` first')
    process.exit(1)
  }
  const digest = new Bun.CryptoHasher('sha256')
    .update(new Uint8Array(await source.arrayBuffer()))
    .digest('hex')
    .slice(0, 12)
  const name = `agent_gossip_wasm_client_bg.${digest}.wasm`
  // Streamed rather than re-using the buffer above, so the 6.6 MB is not held
  // in the JS heap while the bundler runs.
  await Bun.write(`${DIST}app/${name}`, source)
  return `/app/${name}`
}

// Independent and disjoint — the copy writes dist/, the wasm writes dist/app/,
// and the version only reads a manifest. Serially they were ~40 MB of I/O one
// after another for no reason.
const [, wasmPath, version] = await Promise.all([
  // Verbatim, not bundled: index.html is hand-written and byte-served, and the
  // video encodes are the reason the server answers range requests at all.
  cp(here('server/public/'), DIST, { recursive: true }),
  stageWasm(),
  crateVersion(),
])

const result = await Bun.build({
  entrypoints: [here('app/index.html')],
  outdir: `${DIST}app/`,
  target: 'browser',
  minify: Bun.env['NODE_ENV'] !== 'development',
  define: {
    __APP_VERSION__: JSON.stringify(version),
    __WASM_PATH__: JSON.stringify(wasmPath),
  },
  // The shell is served at `/room/` and at every `/<mesh-id>`, but its chunks
  // live under `/app/`. Relative asset paths would resolve against the room's
  // depth and 404, so they have to be absolute.
  publicPath: '/app/',
})

if (!result.success) {
  for (const log of result.logs) console.error(log)
  process.exit(1)
}

console.log(`built ${result.outputs.length} app files; server/dist/ is ready to serve`)
