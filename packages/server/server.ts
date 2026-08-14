import { join, normalize } from 'node:path'

import { isMeshId } from '@agent-gossip/app/lib/meshId.ts'

// The document root, and deliberately not the directory this file sits in:
// everything reachable over HTTP is built into dist/, so server.ts, the app
// sources, package.json and .env all stay outside it and the guard below turns
// them into a 403.
//
// dist/ is produced by `bun run build`: public/ copied verbatim, plus the
// bundled app. It is gitignored, so a fresh checkout must build before serving.
const ROOT = new URL('./dist/', import.meta.url).pathname
const PORT = Number(process.env['PORT']) || 3000

// The app shell, served for the routes the client owns rather than for a file
// on disk.
const SHELL = 'app/index.html'

const RANGE = /^bytes=(\d*)-(\d*)$/

// Content-addressed or content-stable and referenced by name.
const CACHE_IMMUTABLE = 'public, max-age=31536000, immutable'
// s-maxage is what lets Cloudflare hold index.html too; the short max-age keeps
// browsers rechecking, so a post-deploy purge reaches visitors within the
// minute rather than whenever their cache expires.
const CACHE_REVALIDATE = 'public, max-age=60, s-maxage=31536000'

// Safari refuses to start a <video> unless the server answers a range request
// with a 206, and seeking is broken everywhere without one. The demos are
// minutes long, so this is not optional.
function ranged(file: Bun.BunFile, header: string): Response {
  const size = file.size
  const match = RANGE.exec(header.trim())
  if (!match) return new Response('Range Not Satisfiable', { status: 416 })

  const [, rawStart = '', rawEnd = ''] = match
  // `bytes=-500` means the last 500 bytes, not "from 0 to 500".
  const start = rawStart === '' ? size - Number(rawEnd) : Number(rawStart)
  const end = rawStart === '' || rawEnd === '' ? size - 1 : Number(rawEnd)

  if (!(start >= 0 && end < size && start <= end)) {
    return new Response('Range Not Satisfiable', {
      status: 416,
      headers: { 'Content-Range': `bytes */${size}` },
    })
  }

  return new Response(file.slice(start, end + 1), {
    status: 206,
    headers: {
      'Content-Range': `bytes ${start}-${end}/${size}`,
      'Accept-Ranges': 'bytes',
      // Set here rather than in the block below, which this branch returns
      // before reaching. Range requests are how a browser actually fetches the
      // demos, so without this the one thing that caching was written for is
      // the one thing that never got it.
      'Cache-Control': CACHE_IMMUTABLE,
    },
  })
}

Bun.serve({
  port: PORT,
  async fetch(req: Request): Promise<Response> {
    let pathname = decodeURIComponent(new URL(req.url).pathname)
    if (pathname.endsWith('/')) pathname += 'index.html'

    const filePath = normalize(join(ROOT, pathname))
    if (!filePath.startsWith(ROOT)) {
      console.log(`403 ${req.method} ${pathname}`)
      return new Response('Forbidden', { status: 403 })
    }

    let file = Bun.file(filePath)
    if (!(await file.exists())) {
      // Nothing on disk — the client may still own this path. Two cases, and
      // both are served the same shell:
      //
      //   /room, /room/…   the whole app namespace, whatever the client routes
      //                    under it
      //   /<id>            a room, but only if the segment really is a mesh id
      //
      // The prefix is a plain match because that subtree is the app's, and
      // nothing static lives there. The bare id at the root cannot be: the site
      // owns paths there (/style.css, /og.png, /video/…), so a loose pattern
      // would shadow them, and a one-character typo in a link has to 404 rather
      // than open a room that can never connect. Hence a real base58check
      // rather than a character class — which settles reserved words for free,
      // since /about cannot pass a checksum and so needs no denylist.
      const segments = pathname.split('/').filter(Boolean)
      const only = segments.length === 1 ? segments[0] : undefined
      const isShell =
        pathname === '/room' ||
        pathname.startsWith('/room/') ||
        (only !== undefined && (await isMeshId(only)))

      if (!isShell) {
        console.log(`404 ${req.method} ${pathname}`)
        return new Response('Not Found', { status: 404 })
      }

      file = Bun.file(join(ROOT, SHELL))
      if (!(await file.exists())) {
        console.log(`503 ${req.method} ${pathname} (app not built)`)
        return new Response('App not built — run `bun run build`', { status: 503 })
      }
      console.log(`200 ${req.method} ${pathname} -> ${SHELL}`)
      // Deliberately not `immutable` like the hashed chunks: this one file is
      // served under every room path, and it changes on deploy.
      return new Response(file, { headers: { 'Cache-Control': CACHE_REVALIDATE } })
    }

    const range = req.headers.get('range')
    if (range) {
      const res = ranged(file, range)
      console.log(`${res.status} ${req.method} ${pathname} ${range}`)
      return res
    }

    console.log(`200 ${req.method} ${pathname}`)
    // Content-Type is inferred from the extension.
    const res = new Response(file, { headers: { 'Accept-Ranges': 'bytes' } })
    // The shell keeps its name across builds, so it is the one thing under
    // /app/ that must stay revalidated — everything beside it is content-hashed
    // by the bundler.
    const immutable =
      pathname.startsWith('/video/') || (pathname.startsWith('/app/') && pathname !== `/${SHELL}`)
    res.headers.set('Cache-Control', immutable ? CACHE_IMMUTABLE : CACHE_REVALIDATE)
    return res
  },
})

console.log(`listening on :${PORT}`)
