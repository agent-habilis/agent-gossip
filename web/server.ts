import { join, normalize } from 'node:path'

// The document root, and deliberately not the directory this file sits in:
// everything reachable over HTTP lives under src/, so server.ts, package.json
// and .env stay outside it and the guard below turns them into a 403.
const ROOT = new URL('./src/', import.meta.url).pathname
const PORT = Number(process.env['PORT']) || 3000

const RANGE = /^bytes=(\d*)-(\d*)$/

// Safari refuses to start a <video> unless the server answers a range request
// with a 206, and seeking is broken everywhere without one. The demos are
// minutes long, so this is not optional.
function ranged(file: Bun.BunFile, size: number, header: string): Response {
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

    const file = Bun.file(filePath)
    if (!(await file.exists())) {
      console.log(`404 ${req.method} ${pathname}`)
      return new Response('Not Found', { status: 404 })
    }

    const range = req.headers.get('range')
    if (range) {
      const res = ranged(file, file.size, range)
      console.log(`${res.status} ${req.method} ${pathname} ${range}`)
      return res
    }

    console.log(`200 ${req.method} ${pathname}`)
    // Content-Type is inferred from the extension.
    const res = new Response(file, { headers: { 'Accept-Ranges': 'bytes' } })
    // Encodes are content-stable and referenced by name, so a visitor who
    // scrubs a demo never refetches it.
    if (pathname.startsWith('/video/')) {
      res.headers.set('Cache-Control', 'public, max-age=31536000, immutable')
    } else {
      // s-maxage is what lets Cloudflare hold index.html too; the short
      // max-age keeps browsers rechecking, so a post-deploy purge reaches
      // visitors within the minute rather than whenever their cache expires.
      res.headers.set('Cache-Control', 'public, max-age=60, s-maxage=31536000')
    }
    return res
  },
})

console.log(`listening on :${PORT}`)
