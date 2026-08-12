# agent-gossip.com

Static site served from a tiny zero-dependency Bun + TypeScript server, exposed
publicly through a Cloudflare Tunnel. Both processes run as containers via
`docker compose`. Same shape as
[agent-habilis.com](https://agent-habilis.com), which is where the stylesheet
comes from — with the classic HTML palette (white ground, black text, blue
links, purple visited) in place of that site's colours, and no dark mode,
because the classic palette never had one.

There is no build step and no client-side JavaScript. `src/index.html` and
`src/style.css` are hand-written and served byte-for-byte off disk.

`src/` is the document root, and nothing outside it is reachable over HTTP —
which is why `server.ts` sits beside `src/` rather than inside it. Were the
server in its own document root, `/server.ts`, `/package.json` and `/.env`
would all be fetchable.

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
bun start          # bun server.ts
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

`src/video/` holds web-sized re-encodes of the screen recordings in `../assets`,
plus a poster frame for each. The originals are ~338 MB of high-bitrate capture;
the encodes are ~32 MB total. Regenerate with:

```sh
bun run media   # ./scripts/encode-media.sh
```

The page loads them with `preload="none"`, so a visitor downloads only posters
until they press play. `server.ts` answers HTTP range requests — without a `206`
Safari will not start a `<video>` at all, and seeking breaks everywhere.

`src/og.png` is rasterized from `src/og.svg` with headless Chrome (no SVG
rasterizer CLI is assumed to be installed, and crawlers do not reliably accept
SVG cards):

```sh
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless --disable-gpu --hide-scrollbars \
  --screenshot=src/og.png --window-size=1200,630 "file://$PWD/src/og.svg"
```

## Layout

- `src/` — the document root; everything served, and nothing else
- `src/index.html`, `src/style.css` — files served at the site root
- `src/og.svg` / `src/og.png` — the social card and its source
- `src/video/` — the demo encodes and their poster frames
- `server.ts` — zero-dep static server (`Bun.serve` + `Bun.file`), with range support
- `scripts/encode-media.sh` — re-encodes `../assets/*.mp4` into `src/video/`
- `Dockerfile` — `oven/bun:alpine` image
- `docker-compose.yml` — `agent-gossip-com` + `cloudflared` services
- `.env` — local-only, holds `CLOUDFLARE_TUNNEL_TOKEN` (never committed)
