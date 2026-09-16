import { afterAll, beforeAll, describe, expect, test } from 'bun:test'
import { rm } from 'node:fs/promises'

import { GOLDEN_MESH_ID } from '@agent-gossip/app/lib/meshId.ts'

// Runs the real server against its real document root, so the fixtures are
// written into dist/ under a name no build produces, and removed afterwards.
const DIST = new URL('./dist/', import.meta.url).pathname
const FIXTURE = '__server-test__'
const CHUNK = `_next/static/${FIXTURE}.js`
const PORT = 20000 + Math.floor(Math.random() * 20000)

let server: ReturnType<typeof Bun.spawn>

// Bun's own fetch, not the global: the workspace preload registers happy-dom,
// whose fetch replaces it and never reaches a real socket.
const get = (path: string) => Bun.fetch(`http://127.0.0.1:${PORT}${path}`, { redirect: 'manual' })

beforeAll(async () => {
  await Bun.write(`${DIST}${FIXTURE}/page/index.html`, '<p>fixture</p>')
  await Bun.write(`${DIST}${CHUNK}`, '')
  server = Bun.spawn(['bun', new URL('./server.ts', import.meta.url).pathname], {
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

  // 200 once the app is built, 503 before: either way the shell was chosen.
  test('still routes /room and a mesh id to the app shell', async () => {
    for (const path of ['/room', '/room/', `/${GOLDEN_MESH_ID}`]) {
      expect({ path, status: (await get(path)).status }).not.toEqual({ path, status: 404 })
    }
  })

  test("caches Next's content-hashed chunks forever", async () => {
    const res = await get(`/${CHUNK}`)
    expect(res.headers.get('cache-control')).toContain('immutable')
  })
})
