# agent-gossip.com

Static site served from a tiny zero-dependency Bun + TypeScript server, exposed
publicly through a Cloudflare Tunnel. Both processes run as containers via
`docker compose`. Same shape as
[agent-habilis.com](https://agent-habilis.com), which is where the stylesheet
comes from — with the classic HTML palette (white ground, black text, blue
links, purple visited) in place of that site's colours, and no dark mode,
because the classic palette never had one.

There is no build step and no client-side JavaScript. `index.html` and
`style.css` are hand-written and served byte-for-byte off disk.

## Prerequisites

- Docker Desktop (or any Docker engine with the Compose v2 plugin)
- A Cloudflare account with a tunnel created in the Zero Trust dashboard
  (Networks -> Tunnels -> Create a tunnel -> Cloudflared)
- [Bun](https://bun.sh) >= 1.3 (only for running/type-checking outside Docker)
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

## Stop

```sh
docker compose down
```

## Media

`video/` holds web-sized re-encodes of the screen recordings in `../assets`,
plus a poster frame for each. The originals are ~338 MB of high-bitrate capture;
the encodes are ~32 MB total. Regenerate with:

```sh
bun run media   # ./encode-media.sh
```

The page loads them with `preload="none"`, so a visitor downloads only posters
until they press play. `server.ts` answers HTTP range requests — without a `206`
Safari will not start a `<video>` at all, and seeking breaks everywhere.

`og.png` is rasterized from `og.svg` with headless Chrome (no SVG rasterizer CLI
is assumed to be installed, and crawlers do not reliably accept SVG cards):

```sh
"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  --headless --disable-gpu --hide-scrollbars \
  --screenshot=og.png --window-size=1200,630 "file://$PWD/og.svg"
```

## Layout

- `server.ts` — zero-dep static server (`Bun.serve` + `Bun.file`), with range support
- `index.html`, `style.css` — files served at the site root
- `encode-media.sh` — re-encodes `../assets/*.mp4` into `video/`
- `og.svg` / `og.png` — the social card and its source
- `Dockerfile` — `oven/bun:alpine` image
- `docker-compose.yml` — `agent-gossip-com` + `cloudflared` services
- `.env` — local-only, holds `CLOUDFLARE_TUNNEL_TOKEN` (never committed)
