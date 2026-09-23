/**
 * Bundles `webapp/` into `public/app/`, where Next serves it verbatim — in
 * `next dev` and in the static export alike.
 *
 * This is a second bundler inside a Next package on purpose: `webapp/` is
 * visage-dom JSX, and Next's pipeline is React. Keeping it behind Bun.build
 * means the visage sources — and the vendored visage and moonspace packages —
 * need no pragmas and no rewrite.
 *
 * Bun reads `jsx` / `jsxImportSource` out of webapp/tsconfig.json, so there is
 * no JSX configuration here.
 */
import { readdir, rm } from 'node:fs/promises'
import { watch } from 'node:fs'
import { fileURLToPath } from 'node:url'

const here = (path: string) => fileURLToPath(new URL(`../${path}`, import.meta.url))

const OUT = here('public/app/')
const SRC = here('webapp/')

/**
 * Read from the crate rather than duplicated here, so `gossip_version` in the
 * browser cannot drift from the binary's. A drift would be invisible: both
 * would answer, and only one would be right.
 */
async function crateVersion(): Promise<string> {
  const manifest = await Bun.file(here('../../crates/agent-gossip/Cargo.toml')).text()
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
  const source = Bun.file(here('webapp/wasm/pkg/agent_gossip_wasm_client_bg.wasm'))
  if (!(await source.exists())) {
    console.error('missing webapp/wasm/pkg — run `bun run build:wasm` first')
    process.exit(1)
  }
  const digest = new Bun.CryptoHasher('sha256')
    .update(new Uint8Array(await source.arrayBuffer()))
    .digest('hex')
    .slice(0, 12)
  const name = `agent_gossip_wasm_client_bg.${digest}.wasm`
  // Named by its own content, so a copy already there is the same 6.6 MB.
  // Skipping it keeps a rebuild off the slowest step, which is what makes the
  // watch loop usable.
  if (!(await Bun.file(`${OUT}${name}`).exists())) {
    // Streamed rather than re-using the buffer above, so the 6.6 MB is not held
    // in the JS heap while the bundler runs.
    await Bun.write(`${OUT}${name}`, source)
  }
  return `/app/${name}`
}

/**
 * Deliberately not an `rm -rf` of the directory up front: `next dev` serves
 * `public/` straight off disk, so emptying it means every rebuild has a window
 * where `/app/main.js` 404s. Writing over the top and sweeping afterwards
 * leaves no such window.
 */
async function prune(keep: Set<string>): Promise<void> {
  const present = await readdir(OUT).catch(() => [])
  await Promise.all(
    present
      .filter((name) => !keep.has(name))
      // `recursive` because a bare `force` still rejects on a directory, and a
      // rejection here would take the watch loop down with it.
      .map((name) => rm(`${OUT}${name}`, { force: true, recursive: true })),
  )
}

async function build(): Promise<boolean> {
  const [wasmPath, version] = await Promise.all([stageWasm(), crateVersion()])

  const result = await Bun.build({
    entrypoints: [here('webapp/main.tsx')],
    outdir: OUT,
    target: 'browser',
    minify: Bun.env['NODE_ENV'] !== 'development',
    define: {
      __APP_VERSION__: JSON.stringify(version),
      __WASM_PATH__: JSON.stringify(wasmPath),
    },
    // The route at /app/ loads these by absolute URL, and chunks are fetched
    // from that same depth. Relative paths would resolve against the page and
    // 404.
    publicPath: '/app/',
    // Unhashed, so the /app/ page can reference /app/main.js without reading a
    // manifest. Everything here is versioned by the deploy, and the one asset
    // that must be cached forever — the wasm — is content-addressed above.
    naming: { entry: '[name].[ext]', chunk: '[name]-[hash].[ext]', asset: '[name]-[hash].[ext]' },
  })

  if (!result.success) {
    for (const log of result.logs) console.error(log)
    return false
  }

  await prune(
    new Set([
      ...result.outputs.map((output) => output.path.split('/').pop() ?? ''),
      wasmPath.replace('/app/', ''),
    ]),
  )
  console.log(`built ${result.outputs.length} webapp files into public/app/`)
  return true
}

if (!Bun.argv.includes('--watch')) {
  if (!(await build())) process.exit(1)
} else {
  // `bun --watch` cannot do this: it re-runs a script when the script's own
  // module graph changes, and webapp/ reaches this file as an entrypoint
  // string rather than an import — so nothing under it was ever watched.
  await build()
  let building = false
  let again = false
  watch(SRC, { recursive: true }, () => {
    if (building) {
      again = true
      return
    }
    building = true
    void (async () => {
      try {
        do {
          again = false
          await build()
        } while (again)
      } catch (error) {
        // Without the catch a single throw leaves `building` true forever and
        // the watcher stops reacting, silently.
        console.error(error)
      } finally {
        building = false
      }
    })()
  })
  console.log(`watching ${SRC}`)
}
