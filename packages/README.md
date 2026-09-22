# agent-gossip.com

The marketing site plus the browser gossip client, served from a tiny
zero-dependency Bun + TypeScript server and exposed publicly through a
Cloudflare Tunnel. Both processes run as containers via `docker compose`.

Everything lives in **`web/`**, one [Nextra](https://nextra.site) site (Next.js,
React) built as a static export, with **Nextra driving every route**:

| route | content |
|---|---|
| `/` | the landing page (`web/content/index.mdx`) |
| `/docs/…` | the docs, hand-written MDX under `web/content/docs/` |
| `/app/` | the gossip web app |

The full CLI reference stays in `agent-gossip man`. Search is
[Pagefind](https://pagefind.app), indexed by a `postbuild` step, so it works in
`next dev` only after one `bun run build` in `web/`.

`/app/` is the one place two worlds meet. The app is a
[visage](README-vendored.md) SPA — its own JSX runtime and reconciler, not
React — so Next does not render it. `web/scripts/build-webapp.ts` bundles
`web/webapp/` with Bun into `web/public/app/`, and the route at
`web/app/app/page.tsx` is a mount point that pulls that bundle in. Two bundlers
in one package, which is the price of keeping the visage sources unchanged and
the vendored libraries free of pragmas.

A room is `/app/?mesh=<id>`, not `/<mesh-id>`: a static export has no server
that could resolve an arbitrary path into a shell. `web/webapp/lib/route.ts` is
the whole router.

`web/out/` is the document root and is gitignored, so a fresh checkout must
build before it can serve:

```sh
bun install
bun run build     # webapp bundled into web/public/app/, then next build -> web/out/
bun run serve
```

`bun start` does both.

## Routing

The server no longer routes — it returns files. Every URL that exists is one the
export wrote, so there is no shell fallback and no mesh-id check:

| request | served |
|---|---|
| any path | the file in `web/out/` |
| `/docs/x` (no slash) | `308` to `/docs/x/`, when `docs/x/index.html` exists |
| anything else | `404` |

## Tests

```sh
bun test        # unit — happy-dom, milliseconds
bun run e2e     # end-to-end — a real Chrome via agent-browse, seconds
```

The e2e suite needs a built `web/out/` and a running server, and targets
`https://agent-gossip.localhost` (override with `E2E_BASE`). Chrome for Testing
trusts portless's CA, so the HTTPS alias works as-is — which matters, because a
secure context is what lets the wasm client use `crypto.subtle` and WebRTC.

Two things it does deliberately:

- **Every navigation is cache-busting.** The app shell is served with
  `max-age=60`, so a plain reload will hand back the previous build's chunk and
  make a green run mean nothing.
- **It builds and drives the bundle that ships.** There is no test-only define:
  a suite that rebuilt with a seam switched on would be green against a build no
  user ever gets. WebMCP is a precondition — an older browser fails the case with
  "needs Chrome 150 or newer" rather than being shimmed around.

The two-tab section is the acceptance test: one browser creates a gossip, another
joins it over WebRTC, and messages cross in both directions.

## Driving the page with an agent

The app publishes the **same 19 tools** as `agent-gossip mcp` — same names, same
schemas — as [WebMCP](https://webmachinelearning.github.io/webmcp/) tools, so an
agent can drive a tab without a binary on the box. A test pins the two lists
against each other, because the whole value is that a skill written for one
drives the other.

Registration lives in `app/lib/agentTools/`. It is feature-detected: on a
browser without WebMCP — which today is every browser by default — it reads one
property and does nothing.

```bash
claude mcp add chrome-devtools-webmcp --scope user -- \
  npx -y chrome-devtools-mcp@latest \
  --categoryExperimentalWebmcp \
  --chromeArg=--enable-features=WebMCP
```

Both flags are load-bearing and neither implies the other on a browser that
still gates the feature. **Chrome for Testing 152 ships WebMCP on by default** —
`document.modelContext` is simply there, no flag — so `bun run e2e` drives the
real API and the suite covers the bridge end to end. Then `navigate_page` to a
room, `list_webmcp_tools`, `execute_webmcp_tool`.

Four behaviours of Chrome's implementation shape the code, and are the reason it
looks the way it does:

- **Input is never validated against `inputSchema`**, so every tool checks its
  own arguments — and checks them *before* looking for a gossip, or a malformed
  call reports `no_session` and the agent fixes the wrong thing.
- **A throw is flattened** to `UnknownError` with the message stripped, so
  failures are returned as `{ ok: false, code, error }` data instead.
- **Registration is per-call and concurrent**: until the last one lands,
  `getTools()` returns a partial list and says nothing about being incomplete.
- **Unregistering is only possible through an `AbortSignal`** given at
  registration time.
- **A tool's return value is serialized to a string.** Reading a result back
  through `executeTool` means JSON-decoding it, and the e2e helper unwraps
  repeatedly rather than assuming a fixed depth.

Tools that cannot mean the same thing in a tab are refused with a reason rather
than quietly given different semantics — `create_gossip` does not offer the
CLI's `mdns` and `dht` arguments, because a browser has neither.

### Telling the person an agent is here

The top bar shows a badge — `‹AGENT CONTROLLING›` during a call, `‹AGENT ACTIVE›`
for fifteen seconds after, then a muted count.

The wording is careful, because **the thing you would want to show cannot be
observed**. WebMCP lets a page publish tools; it never tells the page something
connected to them, and the spec has no notion of an agent session. A tab whose
tools nobody has called is indistinguishable from a tab no agent has found. So
the badge is built from the only real evidence — a call actually happening — and
never claims more. Do not "improve" it into a connected/disconnected indicator;
there is nothing to drive one with.

A password is never written into the call log: `join_gossip` and `create_gossip`
both take one, the log is drawn on the page, and a plain `JSON.stringify` of the
arguments would put it on screen and into any screenshot. A test covers it.

## Prerequisites

- Docker Desktop (or any Docker engine with the Compose v2 plugin)
- A Cloudflare account with a tunnel created in the Zero Trust dashboard
  (Networks -> Tunnels -> Create a tunnel -> Cloudflared)
- [Bun](https://bun.sh) >= 1.3 (only for running/type-checking outside Docker)
- Node >= 24, for the `portless` CLI only — the package itself arrives with
  `bun install`
- `ffmpeg` (only for regenerating the demo videos)

## First-time setup

1. In the Cloudflare Zero Trust dashboard, create a tunnel and copy the
   connector token shown on the "Install and run a connector" step.
2. In the same tunnel, add a public hostname route pointing `agent-gossip.com`
   at the service URL `http://agent-gossip-com:3000`.
3. Locally:

   ```sh
   cp .env.example .env
   # paste the token into .env
   ```

## Run

```sh
docker compose up -d
```

The container binds `127.0.0.1:3001` on the host — 3000 is already taken by
`agent-habilis-com`, so the two sites can run side by side.

```sh
curl http://localhost:3001/
docker compose logs agent-gossip-com
docker compose logs cloudflared
```

To run the server directly (no Docker, port 3000):

```sh
bun install
bun start          # build, then bun server.ts
bun run type-check # tsc, no emit
```

Or under [portless](https://github.com/vercel-labs/portless), which gives the
site a name instead of a port — useful because 3000 is contended and every
browser check otherwise has to be told which port won:

```sh
bun run dev        # https://agent-gossip.localhost
```

The name and the wrapped script live in the `"portless"` key of `package.json`.
portless assigns a free port in 4000-4999 and passes it as `PORT`, which
`server.ts` already reads, so nothing in the server changes. The first run costs
more than a config edit: it starts a background proxy daemon, generates a local
CA and adds it to the system trust store, binds port 443 (so it asks for sudo),
and writes into `/etc/hosts`. `bunx portless clean` undoes all of it.

Safari resolves `.localhost` through the system resolver rather than natively,
so if the name does not load there, `bunx portless hosts sync`.

## Stop

```sh
docker compose down
```

## Media

`public/video/` holds web-sized re-encodes of the screen recordings in `../assets`,
plus a poster frame for each. The originals are ~338 MB of high-bitrate capture;
the encodes are ~32 MB total. Regenerate with:

```sh
bun run media           # re-encode every clip, then cut its posters
bun run media posters   # only re-cut the posters, from the existing encodes
```

The page loads them with `preload="none"`, so a visitor downloads only posters
until they press play. `server.ts` answers HTTP range requests — without a `206`
Safari will not start a `<video>` at all, and seeking breaks everywhere.

`public/og.png` is rasterized from `public/og.svg` with headless Chrome (no SVG
rasterizer CLI is assumed to be installed, and crawlers do not reliably accept
SVG cards):

```sh
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless --disable-gpu --hide-scrollbars \
  --screenshot=public/og.png --window-size=1200,630 "file://$PWD/public/og.svg"
```

## Layout

- `web/out/` — the document root; everything served, and nothing else. Built, gitignored
- `web/server.ts` — zero-dep static server (`Bun.serve` + `Bun.file`), with range support
- `web/app/` — the Next routes. `layout.tsx` is the bare document shell;
  `(site)/` adds the docs chrome; `app/` is the webapp mount point, outside that
  group so the SPA gets the whole viewport
- `web/content/` — the landing page and docs as MDX
- `web/public/` — the media, og image and favicon, served as-is. `public/app/`
  is the built webapp bundle, gitignored
- `web/webapp/` — the gossip app: `main.tsx`, `pages/` (`home/` is the front
  door, `room/` a joined mesh), `components/`, `lib/`, `wasm/`. Its own
  `tsconfig.json`, which is what makes Bun compile it as visage JSX
- `web/scripts/build-webapp.ts` — bundles `webapp/` and stages the wasm into `public/app/`
- `visage-*` / `moonspace-*` — vendored as source. See `README-vendored.md`
- `scripts/build.ts` — runs `bun run build` in `web/`
- `scripts/build-wasm.ts` — builds `crates/agent-gossip-wasm-client` and runs `wasm-bindgen`
- `scripts/e2e.ts` — the browser suite; `scripts/test-setup.ts` — happy-dom preload
- `scripts/encode-media.ts` — re-encodes `../assets/*.mp4` into `web/public/video/`
- `Dockerfile` — `oven/bun:alpine` image
- `docker-compose.yml` — `agent-gossip-com` + `cloudflared` services
- `.env` — local-only, holds `CLOUDFLARE_TUNNEL_TOKEN` (never committed)
