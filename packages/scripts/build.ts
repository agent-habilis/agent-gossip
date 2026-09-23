/**
 * Produces `web/out/`, the served document root.
 *
 * There is no assembly step left: Next's static export already carries
 * `web/public/` — the media, the icons and the webapp bundle — inside `out/`.
 * This script exists only so the repo root still has one `bun run build`.
 *
 * `web`'s own `prebuild` runs `scripts/build-webapp.ts`, which is what puts the
 * webapp under `public/app/` before the export copies it.
 */

// Only when absent: the Docker image has no Rust toolchain and relies on the
// `pkg/` copied in from the host, and a warm rebuild should not pay for cargo.
const wasm = new URL('../web/webapp/wasm/pkg/agent_gossip_wasm_client_bg.wasm', import.meta.url)
if (!(await Bun.file(wasm).exists())) {
  const buildWasm = Bun.spawn(['bun', 'run', 'build:wasm'], {
    cwd: new URL('../', import.meta.url).pathname,
    stdout: 'inherit',
    stderr: 'inherit',
  })
  if ((await buildWasm.exited) !== 0) {
    console.error('wasm build failed')
    process.exit(1)
  }
}

const build = Bun.spawn(['bun', 'run', 'build'], {
  cwd: new URL('../web/', import.meta.url).pathname,
  stdout: 'inherit',
  stderr: 'inherit',
})

if ((await build.exited) !== 0) {
  console.error('web build failed')
  process.exit(1)
}

console.log('web/out/ is ready to serve')
