import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

// Runs the real server against its real document root, so the fixtures are
// written into out/ under a name no build produces, and removed afterwards.
const DIST = fileURLToPath(new URL('./out/', import.meta.url))
const FIXTURE = '__server-test__'
const CHUNK = `_next/static/${FIXTURE}.js`
const PORT = 20000 + Math.floor(Math.random() * 20000)

let server: ReturnType<typeof Bun.spawn>

// Bun's own fetch, not the global: the workspace preload registers happy-dom,
// whose fetch replaces it and never reaches a real socket.
const get = (path: string, headers?: Record<string, string>) =>
  Bun.fetch(`http://127.0.0.1:${PORT}${path}`, { headers, redirect: 'manual' })

beforeAll(async () => {
  await Bun.write(`${DIST}${FIXTURE}/page/index.html`, '<p>fixture</p>')
  await Bun.write(`${DIST}${CHUNK}`, '')
  server = Bun.spawn(['bun', fileURLToPath(new URL('./server.ts', import.meta.url))], {
    env: { ...process.env, PORT: String(PORT) },
    stdout: 'ignore',
  })
  for (let attempt = 0; ; attempt++) {
    try {
      await get('/')
      return
    } catch (error) {
      if (attempt > 100) throw error
      await Bun.sleep(50)
    }
  }
})

afterAll(async () => {
  server.kill()
  await rm(`${DIST}${FIXTURE}`, { recursive: true, force: true })
  await rm(`${DIST}${CHUNK}`, { force: true })
})

describe('server', () => {
  test('redirects a directory path to its slashed form', async () => {
    const res = await get(`/${FIXTURE}/page?q=1`)
    expect(res.status).toBe(308)
    expect(res.headers.get('location')).toBe(`/${FIXTURE}/page/?q=1`)
  })

  test('serves the directory index at the slashed path', async () => {
    const res = await get(`/${FIXTURE}/page/`)
    expect(res.status).toBe(200)
    expect(await res.text()).toBe('<p>fixture</p>')
  })

  test('still 404s a path with neither a file nor an index', async () => {
    expect((await get(`/${FIXTURE}/nope`)).status).toBe(404)
  })

  // Nextra routes the whole site now, so there is no dynamic path left to
  // serve: a bare segment that no exported file matches is simply not a page.
  test('404s a bare segment rather than falling back to an app shell', async () => {
    expect((await get('/room')).status).toBe(404)
  })

  test("caches Next's content-hashed chunks forever", async () => {
    const res = await get(`/${CHUNK}`)
    expect(res.headers.get('cache-control')).toContain('immutable')
  })

  // A Location built from the request rather than from disk is an open
  // redirect: `%5C` decodes to a backslash, which the URL standard reads as a
  // slash, so `/\evil.com/../x/` resolves against evil.com. Asserting the shape
  // rather than one payload is what makes this catch the next variant too.
  test('never puts a request-controlled host in a redirect', async () => {
    for (const path of [
      `/%5Cevil.com%2f..%2f${FIXTURE}%2fpage`,
      `//${FIXTURE}/page`,
      `/${FIXTURE}%2f%0d%0aX-Injected:1%2f..%2f${FIXTURE}/page`,
    ]) {
      const location = (await get(path)).headers.get('location')
      if (location === null) continue
      expect({ path, location }).toEqual({ path, location: `/${FIXTURE}/page/` })
    }
  })

  test('rejects a malformed path rather than throwing', async () => {
    expect((await get('/%E0%A4%A')).status).toBe(400)
    expect((await get('/docs%00')).status).toBe(400)
  })

  // The guard is on the resolved file, so an encoded traversal cannot reach
  // outside the document root whatever it spells.
  test('keeps an encoded traversal inside the document root', async () => {
    expect((await get('/..%2fserver.ts')).status).toBe(403)
    expect((await get('/../server.ts')).status).toBe(404)
  })

  // The policy has to read the resolved path, not the requested one, or a
  // traversal back out of /video/ borrows its year-long immutable header.
  test('decides the cache policy on the resolved path', async () => {
    const res = await get(`/video/..%2f${CHUNK}%2f..%2f..%2f..%2f${FIXTURE}/page/`)
    expect(res.status).toBe(200)
    expect(res.headers.get('cache-control')).not.toContain('immutable')
  })

  test('caches a page by name but not a range of it', async () => {
    const res = await get(`/${FIXTURE}/page/`, { Range: 'bytes=0-3' })
    expect(res.status).toBe(206)
    expect(res.headers.get('cache-control')).not.toContain('immutable')
  })

  // A shared cache must not hold a miss: the file it is missing is usually one
  // the next deploy adds.
  test('does not let a shared cache pin a 404', async () => {
    const res = await get(`/${FIXTURE}/nope`)
    expect(res.status).toBe(404)
    expect(res.headers.get('cache-control') ?? '').not.toContain('s-maxage=31536000')
  })
})
