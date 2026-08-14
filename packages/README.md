# agent-gossip.com

The marketing site plus the browser gossip client, served from a tiny
zero-dependency Bun + TypeScript server and exposed publicly through a
Cloudflare Tunnel. Both processes run as containers via `docker compose`.

Two halves, deliberately unalike:

- **`public/`** — the marketing site. Hand-written `index.html` and `style.css`,
  copied byte-for-byte into the build. No bundler touches them, and there is no
  client-side JavaScript on that page.
- **`src/`** — the gossip web app, at `/room/` and at every `/<mesh-id>`. A
  [visage](README-vendored.md) SPA bundled by `scripts/build.ts`.

Both land in **`dist/`, which is the document root**. Nothing outside it is
reachable over HTTP — which is why `server.ts`, `src/`, `public/` and `vendor/`
all sit beside it rather than inside it. Were the server in its own document
root, `/server.ts`, `/package.json` and `/.env` would all be fetchable, and so
would every `.tsx` file in the app.

`server/dist/` is gitignored, so a fresh checkout must build before it can serve:

```sh
bun install
bun run build     # server/public/ copied + app/ bundled -> server/dist/
bun run serve
```

`bun start` does both. The server answers `503` rather than `404` when `dist/`
has no app in it, so "you forgot to build" does not look like a routing bug.

## Routing

| request | served |
|---|---|
| `/`, `/style.css`, `/video/…` | the file in `server/dist/` |
| `/room`, `/room/…` | the app shell — the whole subtree is the app's |
| `/<mesh-id>` | the app shell, **only if the segment is a valid mesh id** |
| anything else | `404` |

That last rule is a real base58check over the id, not a character class
(`app/lib/meshId.ts`, shared with the app's join form so the two cannot
disagree). The site owns paths at the root, so a loose pattern would shadow
`/style.css`; and a one-character typo in a shared link has to 404 rather than
open a room that can never connect. It also settles reserved words for free —
`/about` cannot pass a checksum, so there is no denylist to maintain.

## Tests

```sh
bun test        # unit — happy-dom, milliseconds
bun run e2e     # end-to-end — a real Chrome via agent-browse, seconds
```

The e2e suite needs a built `dist/` and a running server, and targets
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
bun run media   # ./scripts/encode-media.sh
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

- `server/dist/` — the document root; everything served, and nothing else. Built, gitignored
- `server/server.ts` — zero-dep static server (`Bun.serve` + `Bun.file`), with range support
- `server/public/` — the marketing site, copied verbatim into `server/dist/`
- `app/` — the gossip app: `main.tsx`, `pages/` (laid out to mirror the URLs),
  `components/`, `lib/`, `wasm/`. Bundled into `server/dist/app/`
- `visage-*` / `moonspace-*` — vendored as source. See `README-vendored.md`
- `scripts/build.ts` — copies `server/public/`, bundles `app/`
- `scripts/build-wasm.ts` — builds `crates/agent-gossip-wasm-client` and runs `wasm-bindgen`
- `scripts/e2e.ts` — the browser suite; `scripts/test-setup.ts` — happy-dom preload
- `scripts/encode-media.sh` — re-encodes `../assets/*.mp4` into `server/public/video/`
- `Dockerfile` — `oven/bun:alpine` image
- `docker-compose.yml` — `agent-gossip-com` + `cloudflared` services
- `.env` — local-only, holds `CLOUDFLARE_TUNNEL_TOKEN` (never committed)
