import { join, normalize, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

/** A path built from disk, made safe to put in a Location header. */
const encodePath = (path: string) => path.split('/').map(encodeURIComponent).join('/')

// The document root, and deliberately not the directory this file sits in:
// everything reachable over HTTP is exported into out/, so server.ts, the
// webapp sources, package.json and .env all stay outside it and the guard below
// turns them into a 403.
//
// out/ is produced by `bun run build`: the Next static export, which already
// carries public/ — media, icons and the bundled webapp — inside it. It is
// gitignored, so a fresh checkout must build before serving.
const ROOT = fileURLToPath(new URL('./out/', import.meta.url))
const PORT = Number(process.env['PORT']) || 3000

const RANGE = /^bytes=(\d*)-(\d*)$/

// Content-addressed or content-stable and referenced by name.
const CACHE_IMMUTABLE = 'public, max-age=31536000, immutable'
// s-maxage is what lets Cloudflare hold index.html too; the short max-age keeps
// browsers rechecking, so a post-deploy purge reaches visitors within the
// minute rather than whenever their cache expires.
const CACHE_REVALIDATE = 'public, max-age=60, s-maxage=31536000'
// A miss is the one answer a shared cache must not hold: the file it is missing
// is usually one a deploy is about to add.
const CACHE_MISS = 'public, max-age=60'

// Only what provably changes name with its content. The webapp's own entry is
// unhashed so the route can link it by name, so it revalidates like any page;
// the wasm beside it is content-addressed and is the one asset big enough for
// that to matter.
function cachePolicy(pathname: string): string {
  const immutable =
    pathname.startsWith('/video/') ||
    pathname.startsWith('/_next/static/') ||
    (pathname.startsWith('/app/') && pathname.endsWith('.wasm'))
  return immutable ? CACHE_IMMUTABLE : CACHE_REVALIDATE
}

// Safari refuses to start a <video> unless the server answers a range request
// with a 206, and seeking is broken everywhere without one. The demos are
// minutes long, so this is not optional.
function ranged(file: Bun.BunFile, header: string, cacheControl: string): Response {
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
      // Passed in rather than decided here: this branch returns before the
      // block below, and a 206 that stamped `immutable` on whatever was asked
      // for would pin a year of cache onto index.html for any client that sent
      // a Range header.
      'Cache-Control': cacheControl,
    },
  })
}

Bun.serve({
  port: PORT,
  async fetch(req: Request): Promise<Response> {
    const raw = new URL(req.url).pathname
    let pathname: string
    try {
      // Throws on malformed percent-encoding (`/%E0%A4%A`). Unhandled, that is
      // a 500 and a stack trace for anything a scanner cares to send.
      pathname = decodeURIComponent(raw)
    } catch {
      console.log(`400 ${req.method} ${raw}`)
      return new Response('Bad Request', { status: 400 })
    }
    // A NUL cannot appear in a path on disk, and node's path functions silently
    // accept it.
    if (pathname.includes('\0')) {
      console.log(`400 ${req.method} ${raw}`)
      return new Response('Bad Request', { status: 400 })
    }
    if (pathname.endsWith('/')) pathname += 'index.html'

    const filePath = normalize(join(ROOT, pathname))
    if (!filePath.startsWith(ROOT)) {
      console.log(`403 ${req.method} ${pathname}`)
      return new Response('Forbidden', { status: 403 })
    }

    // The one path every branch below reads: derived from disk after
    // normalization, never echoed from the request. Three readers of three
    // slightly different strings is what let `/video/..%2findex.html` be cached
    // as immutable and `/%5Cevil.com%2f..%2fdocs` redirect off-site.
    const canonical = `/${relative(ROOT, filePath)}`

    const file = Bun.file(filePath)
    if (!(await file.exists())) {
      // The site is exported with trailingSlash, so its pages are
      // docs/x/index.html and every link it writes is `/docs/x/`. A hand-typed
      // `/docs/x` is sent there rather than 404ing.
      if (await Bun.file(join(filePath, 'index.html')).exists()) {
        console.log(`308 ${req.method} ${canonical}`)
        return new Response(null, {
          // From `canonical`, never from the request. Echoing the request path
          // is an open redirect: `/%5Cevil.com%2f..%2fdocs` decodes to a
          // backslash, which the URL standard reads as a slash, so
          // `/\evil.com/../docs/` resolves to `https://evil.com/docs/`.
          // Per-segment encoding also keeps CR and LF out of the header.
          headers: { Location: `${encodePath(canonical)}/${new URL(req.url).search}` },
          status: 308,
        })
      }

      // Nothing on disk, and nothing owns paths dynamically any more: Nextra
      // routes the whole site, so every URL that exists is a file the export
      // wrote. A mesh is joined at /app/?mesh=<id>, which is that one exported
      // page plus a query string the server never sees.
      console.log(`404 ${req.method} ${canonical}`)
      const notFound = Bun.file(join(ROOT, '404.html'))
      if (await notFound.exists()) {
        return new Response(notFound, {
          // Deliberately no s-maxage: a shared cache must not pin a miss for a
          // year. A page requested once before the deploy that adds it would
          // stay a 404 until someone purged, and any client could fill the edge
          // with misses under invented paths.
          headers: { 'Cache-Control': CACHE_MISS, 'Content-Type': 'text/html;charset=utf-8' },
          status: 404,
        })
      }
      return new Response('Not Found', { headers: { 'Cache-Control': CACHE_MISS }, status: 404 })
    }

    const cacheControl = cachePolicy(canonical)

    const range = req.headers.get('range')
    if (range) {
      const res = ranged(file, range, cacheControl)
      console.log(`${res.status} ${req.method} ${pathname} ${range}`)
      return res
    }

    console.log(`200 ${req.method} ${pathname}`)
    // Content-Type is inferred from the extension.
    const res = new Response(file, { headers: { 'Accept-Ranges': 'bytes' } })
    res.headers.set('Cache-Control', cacheControl)
    return res
  },
})

console.log(`listening on :${PORT}`)
