/**
 * End-to-end tests against a real Chrome, driven through `agent-browse` over
 * CDP.
 *
 * Separate from `bun test` on purpose: these need a built `dist/`, a running
 * server and a browser, and they take seconds rather than milliseconds. Run
 * with `bun run e2e`.
 *
 * The two-tab section is the acceptance test: it covers creating a gossip in
 * one browser and joining it from another over WebRTC.
 */

import { GOLDEN_MESH_ID, isMeshId } from '@agent-gossip/app/lib/meshId.ts'

const BASE = Bun.env['E2E_BASE'] ?? 'https://agent-gossip.localhost'
const CA = `${Bun.env['HOME']}/.portless/ca.pem`

const GOLDEN = GOLDEN_MESH_ID
const NEAR_MISS = `${GOLDEN.slice(0, -1)}9`

const TAB1 = Bun.env['PWD'] ?? '.'
const TAB2 = `${TAB1}/.e2e-tab2`

type Result = { ok: boolean; pending?: string }
const results: Result[] = []

// Built here rather than assumed — and with no test-only define, so this is the
// bundle that ships. A suite that rebuilt with a seam switched on was green
// against a build no user ever gets.
console.log('building…')
const build = Bun.spawn(['bun', 'scripts/build.ts'], { stdout: 'inherit', stderr: 'inherit' })
if ((await build.exited) !== 0) process.exit(1)

async function browse(args: string[]): Promise<string> {
  return (await run(args)).out
}

async function run(args: string[]): Promise<{ out: string; code: number }> {
  const proc = Bun.spawn(['agent-browse', ...args], { stdout: 'pipe', stderr: 'pipe' })
  const out = await new Response(proc.stdout).text()
  const code = await proc.exited
  return { out, code }
}

/** Evaluate in the page and return the value, or throw with the CDP error. */
async function evaluate(folder: string, expression: string): Promise<unknown> {
  const raw = await browse([
    'cdp',
    '--folder',
    folder,
    'Runtime.evaluate',
    JSON.stringify({ expression, awaitPromise: true, returnByValue: true }),
  ])
  const parsed = JSON.parse(raw) as {
    result?: { value?: unknown; description?: string }
    exceptionDetails?: unknown
  }
  if (parsed.exceptionDetails) throw new Error(parsed.result?.description ?? 'page threw')
  return parsed.result?.value
}

let visit = 0

async function navigate(folder: string, path: string): Promise<void> {
  // Cache-busting via the URL rather than navigate-then-reload: the shell is
  // served with max-age=60, so a plain load can hand back the previous build's
  // chunk and make a green suite mean nothing — but reloading afterwards threw
  // away a complete load, and each one re-fetches 6.6 MB of wasm and pays the
  // 5s splash floor.
  visit += 1
  const url = `${BASE}${path}${path.includes('?') ? '&' : '?'}v=${visit}`
  await browse(['cdp', '--folder', folder, 'Page.navigate', JSON.stringify({ url })])
}

async function waitFor(folder: string, selector: string, timeoutMs = 20_000): Promise<boolean> {
  // `wait` exits non-zero on timeout, which is the only reliable signal — it
  // prints on success too, so testing for output would pass on a timeout.
  const { code } = await run([
    'wait',
    '--folder',
    folder,
    '--selector',
    selector,
    '--timeout',
    String(timeoutMs),
  ])
  return code === 0
}

async function status(path: string): Promise<number> {
  const proc = Bun.spawn(
    ['curl', '-s', '--cacert', CA, '-o', '/dev/null', '-w', '%{http_code}', `${BASE}${path}`],
    { stdout: 'pipe' },
  )
  const code = await new Response(proc.stdout).text()
  await proc.exited
  return Number(code)
}

async function test(name: string, fn: () => Promise<void>): Promise<void> {
  try {
    await fn()
    results.push({ ok: true })
    console.log(`  ok    ${name}`)
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error)
    results.push({ ok: false })
    console.log(`  FAIL  ${name}\n        ${detail}`)
  }
}

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message)
}

/**
 * Chrome serializes a tool's return value to a string, so a result read back
 * through `executeTool` arrives JSON-encoded — and, once it has also been
 * stringified for transport out of the page, doubly so. Unwrap until it is an
 * object rather than guessing a fixed depth.
 */
function toolResult(raw: unknown): Record<string, unknown> {
  let value: unknown = raw
  for (let i = 0; i < 4 && typeof value === 'string'; i += 1) {
    try {
      value = JSON.parse(value)
    } catch {
      break
    }
  }
  if (typeof value !== 'object' || value === null) {
    throw new Error(`not a tool result: ${String(raw)}`)
  }
  return value as Record<string, unknown>
}

function equal(actual: unknown, expected: unknown, what: string): void {
  assert(actual === expected, `${what}: expected ${String(expected)}, got ${String(actual)}`)
}

// ---------------------------------------------------------------- routing

console.log('\nrouting (no browser needed)')

await test('/ serves the marketing page', async () => {
  equal(await status('/'), 200, 'GET /')
})

await test('static assets still serve', async () => {
  equal(await status('/style.css'), 200, 'GET /style.css')
  equal(await status('/video/readme-demo.mp4'), 200, 'GET /video/readme-demo.mp4')
})

await test('the whole /room subtree is the app', async () => {
  for (const path of ['/room', '/room/', '/room/anything']) {
    equal(await status(path), 200, `GET ${path}`)
  }
})

await test('a valid mesh id serves the app', async () => {
  equal(await status(`/${GOLDEN}`), 200, 'GET /<valid id>')
})

await test('a one-character typo 404s rather than opening a dead room', async () => {
  // The check that proves the id test is a real checksum and not a character
  // class — a regex would happily serve this.
  equal(await status(`/${NEAR_MISS}`), 404, 'GET /<near-miss id>')
})

await test('unknown paths and server source are unreachable', async () => {
  for (const path of ['/about', '/server.ts', '/package.json', '/app/main.tsx']) {
    equal(await status(path), 404, `GET ${path}`)
  }
})

// ---------------------------------------------------------------- browser

console.log('\nbrowser')

await browse(['launch', TAB1, `${BASE}/room/`])

await test('the splash shows a spinner and holds for its floor', async () => {
  await navigate(TAB1, '/room/')
  await Bun.sleep(1200)

  const during = await evaluate(
    TAB1,
    `document.querySelector('[data-status=connecting]') ? 'connecting' : 'gone'`,
  )
  equal(during, 'connecting', 'splash at 1.2s')

  // Centred in both axes: the failure this catches is a full-width Stack
  // packing its children at the start edge.
  const centred = await evaluate(
    TAB1,
    `(() => {
      const wrap = document.querySelector('[data-status=connecting]')
      const row = wrap.firstElementChild
      const kids = [...row.children].filter(c => c.tagName !== 'STYLE')
      const box = kids[0].getBoundingClientRect()
      const dx = Math.abs((box.left + kids.at(-1).getBoundingClientRect().right) / 2 - innerWidth / 2)
      const dy = Math.abs((box.top + box.bottom) / 2 - innerHeight / 2)
      return JSON.stringify({ dx: Math.round(dx), dy: Math.round(dy) })
    })()`,
  )
  const { dx, dy } = JSON.parse(String(centred)) as { dx: number; dy: number }
  assert(dx < 30, `splash off-centre horizontally by ${dx}px`)
  assert(dy < 30, `splash off-centre vertically by ${dy}px`)

  await Bun.sleep(5000)
  const after = await evaluate(
    TAB1,
    `document.querySelector('[data-status=connecting]') ? 'connecting' : 'gone'`,
  )
  equal(after, 'gone', 'splash after the floor')
})

await test('the front door offers create and join', async () => {
  await navigate(TAB1, '/room/')
  assert(await waitFor(TAB1, '[data-action=create]'), 'create button never appeared')
  const labels = await evaluate(
    TAB1,
    `JSON.stringify([...document.querySelectorAll('[data-action]')].map(b => b.dataset.action))`,
  )
  const actions = JSON.parse(String(labels)) as string[]
  assert(actions.includes('create'), 'no create action')
  assert(actions.includes('join'), 'no join action')
})

await test('create never hangs — it reports a reason', async () => {
  // The regression this guards: `create` set a "creating…" state with no
  // failure path, so a click sat there forever with nothing to act on.
  await navigate(TAB1, '/room/')
  assert(await waitFor(TAB1, '[data-action=create]'), 'create button never appeared')
  await evaluate(TAB1, `document.querySelector('[data-action=create]').click()`)

  const settled = await evaluate(
    TAB1,
    `(async () => {
      for (let i = 0; i < 60; i += 1) {
        const el = document.querySelector('[data-status]')
        const state = el?.dataset.status
        if (state && state !== 'creating') return state
        await new Promise(r => setTimeout(r, 250))
      }
      return 'stuck:' + (document.querySelector('[data-status]')?.dataset.status ?? 'none')
    })()`,
  )
  // Either it created a gossip, or it said why not. Never neither.
  assert(
    settled === 'failed' || settled === 'ready',
    `create did not settle: ${String(settled)}`,
  )
})

await test('a bad id is refused locally, without navigating', async () => {
  await navigate(TAB1, '/room/')
  assert(await waitFor(TAB1, '[data-action=join]'), 'join button never appeared')

  const outcome = await evaluate(
    TAB1,
    `(async () => {
      document.querySelector('[data-action=join]').click()
      await new Promise(r => setTimeout(r, 100))
      const input = document.querySelector('[data-field=mesh]')
      input.value = 'not-a-real-hash'
      input.dispatchEvent(new Event('input', { bubbles: true }))
      document.querySelector('[data-action=join-submit]').click()
      await new Promise(r => setTimeout(r, 400))
      const err = document.querySelector('[data-error=join]')
      return JSON.stringify({ path: location.pathname, hasError: Boolean(err) })
    })()`,
  )
  const { path, hasError } = JSON.parse(String(outcome)) as { path: string; hasError: boolean }
  equal(path, '/room/', 'stayed on the front door')
  assert(hasError, 'no error shown for a bad id')
})

await test('a valid id navigates to the room URL', async () => {
  await navigate(TAB1, '/room/')
  assert(await waitFor(TAB1, '[data-action=join]'), 'join button never appeared')

  const path = await evaluate(
    TAB1,
    `(async () => {
      document.querySelector('[data-action=join]').click()
      await new Promise(r => setTimeout(r, 100))
      const input = document.querySelector('[data-field=mesh]')
      input.value = ${JSON.stringify(GOLDEN)}
      input.dispatchEvent(new Event('input', { bubbles: true }))
      document.querySelector('[data-action=join-submit]').click()
      await new Promise(r => setTimeout(r, 500))
      return location.pathname
    })()`,
  )
  equal(path, `/${GOLDEN}`, 'room URL')
})

await test('a room never paints a composer before it has joined', async () => {
  await navigate(TAB1, `/${GOLDEN}`)
  await Bun.sleep(2000)
  // The invariant is not "it stays connecting" — it is that the composer only
  // exists once the mesh is joined. A chat window that accepts typing before
  // the transport is up silently drops what you type.
  const state = await evaluate(
    TAB1,
    `(() => {
       const status = document.querySelector('[data-status]')?.dataset.status ?? 'none'
       const composer = Boolean(document.querySelector('[data-field=composer]'))
       return JSON.stringify({ status, composer })
     })()`,
  )
  const { status, composer } = JSON.parse(String(state)) as { status: string; composer: boolean }
  if (composer) equal(status, 'ready', 'composer present without a joined mesh')
})

await test('no console errors on the front door', async () => {
  await navigate(TAB1, '/room/')
  const log = await browse(['watch', '3000', '--folder', TAB1, '--group', 'console'])
  const errors = [...log.matchAll(/"level":"(error)"/g)]
  assert(errors.length === 0, `${errors.length} console error(s)`)
})

// ------------------------------------------------------------------- webmcp

console.log('\nwebmcp bridge')

await test('registration never breaks the page, with or without WebMCP', async () => {
  await navigate(TAB1, '/room/')
  assert(await waitFor(TAB1, '[data-action=create]'), 'the page did not render')
  // Deliberately not asserting whether `document.modelContext` exists: Chrome
  // for Testing shipped it between 149 and 152, and pinning either answer makes
  // this case a tripwire for the browser's release schedule rather than for our
  // code. What must hold either way is that the page renders and registration
  // is silent.
  const log = await browse(['watch', '2000', '--folder', TAB1, '--group', 'console'])
  assert(!log.includes('"level":"error"'), 'registration logged an error')
})

await test('the agent badge is absent until a tool is called', async () => {
  await navigate(TAB1, '/room/')
  assert(await waitFor(TAB1, '[data-action=create]'), 'the page did not render')
  const badge = await evaluate(TAB1, `Boolean(document.querySelector('[data-agent-badge]'))`)
  // Nothing tells a page an agent connected, so a badge before any call would
  // be claiming something unobservable.
  equal(badge, false, 'badge shown with no calls')
})

await test('the tools register and answer a real executeTool call', async () => {
  await navigate(TAB1, '/room/')
  assert(await waitFor(TAB1, '[data-action=create]'), 'the page did not render')

  // Chrome for Testing 152 ships WebMCP on by default, so this drives the real
  // API against the tools the shipped bundle published at load. An older
  // browser is a precondition failure, not something to shim around — a
  // parallel code path would mean the suite never exercised the real one.
  const available = await evaluate(
    TAB1,
    `document.modelContext
       ? document.modelContext.getTools().then((t) => (t.length > 0 ? 'ok' : 'native but empty'))
       : 'no WebMCP — needs Chrome 150 or newer'`,
  )
  equal(available, 'ok', 'webmcp available')

  const listed = await evaluate(
    TAB1,
    `document.modelContext.getTools().then(t => JSON.stringify({
       count: t.length, names: t.map(x => x.name).sort(),
     }))`,
  )
  const { count, names } = JSON.parse(String(listed)) as { count: number; names: string[] }
  // A partial list is the concurrent-registration bug; agent-share saw three of
  // eight from a sequential batch.
  equal(count, 19, 'published tool count')
  assert(names.includes('send_broadcast'), 'send_broadcast missing')
  assert(names.includes('gossip_version'), 'gossip_version missing')

  // A tool that needs no gossip: proves a call round-trips and returns data.
  const version = await evaluate(
    TAB1,
    `document.modelContext.getTools()
       .then(t => document.modelContext.executeTool(t.find(x => x.name === 'gossip_version'), '{}'))
       .then(r => JSON.stringify(r))`,
  )
  const parsedVersion = toolResult(version)
  equal(parsedVersion['ok'], true, 'gossip_version ok')
  equal(parsedVersion['runtime'], 'browser', 'gossip_version runtime')

  // A tool that needs one: proves failure arrives as data, not as a throw.
  const refused = await evaluate(
    TAB1,
    `document.modelContext.getTools()
       .then(t => document.modelContext.executeTool(
         t.find(x => x.name === 'send_broadcast'), JSON.stringify({ text: 'hi' })))
       .then(r => JSON.stringify(r), e => JSON.stringify({ threw: e.message }))`,
  )
  const parsedRefused = toolResult(refused)
  assert(
    !parsedRefused['threw'],
    `send_broadcast threw instead of returning: ${String(parsedRefused['threw'])}`,
  )
  equal(parsedRefused['ok'], false, 'send_broadcast without a gossip')
  equal(parsedRefused['code'], 'no_session', 'failure code')
})

await test('a tool call raises the agent badge', async () => {
  // The badge is the page's only honest evidence an agent is here, and it must
  // appear from an invocation rather than from anything claiming a connection.
  assert(await waitFor(TAB1, '[data-agent-badge]', 5_000), 'badge never appeared after calls')
  const label = await evaluate(TAB1, `document.querySelector('[data-agent-badge]').textContent`)
  assert(String(label).includes('AGENT'), `unexpected badge: ${String(label)}`)
})

/**
 * Send until the other side sees it, up to `attempts`.
 *
 * Not papering over a flake: gossip offers no delivery guarantee, and a
 * broadcast sent while the link to a just-arrived peer is still forming is
 * genuinely lost — measured here, with the roster already reading 1 on both
 * sides. Retrying is what a chat client would do, and the attempt count is
 * reported so a regression from "1" to "3" is still visible.
 */
async function sayUntilSeen(
  from: string,
  to: string,
  text: string,
  attempts = 3,
): Promise<number> {
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    await say(from, text)
    if (await waitFor(to, `[data-text="${text}"]`, 12_000)) return attempt
  }
  return 0
}

/** Type into the composer and press Enter, exactly as a person would. */
async function say(folder: string, text: string): Promise<void> {
  await evaluate(
    folder,
    `(() => {
      const field = document.querySelector('[data-field=composer]')
      field.value = ${JSON.stringify(text)}
      field.dispatchEvent(new Event('input', { bubbles: true }))
      field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
    })()`,
  )
}

// ------------------------------------------------------- the acceptance test

console.log('\ntwo tabs on one gossip')

let roomPath = ''

await test('tab 1 creates a gossip and lands on its room URL', async () => {
  await navigate(TAB1, '/room/')
  assert(await waitFor(TAB1, '[data-action=create]'), 'create button never appeared')
  await evaluate(TAB1, `document.querySelector('[data-action=create]').click()`)
  assert(await waitFor(TAB1, '[data-status=ready]', 60_000), 'never reached a room')
  roomPath = String(await evaluate(TAB1, 'location.pathname'))
  assert(roomPath.length > 40, `expected a room URL, got ${roomPath}`)
  assert(await isMeshId(roomPath.replace(/^\//, '')), `not a valid mesh id: ${roomPath}`)
})

await test('tab 2 opens that URL and both rosters show the other', async () => {
  // agent-browse keys one window per folder, so a second tab means a second
  // folder key — and the directory has to exist before `launch` will take it.
  await Bun.spawn(['mkdir', '-p', TAB2]).exited
  await browse(['launch', TAB2, `${BASE}${roomPath}?nickname=tabtwo`])
  assert(await waitFor(TAB2, '[data-status=ready]', 60_000), 'tab 2 never joined')
  // Both directions, and awaited together: they poll different tabs, so running
  // them in series doubled the worst case for no reason.
  //
  // 90s is ~3x the measured warm convergence (15-30s). Raising it further does
  // not help: a mesh created seconds earlier has been observed not to converge
  // browser-to-browser at all, even at 150s, while a CLI peer joins the same
  // mesh instantly. That is a discovery problem in the client, not impatience
  // here — do not tune this number hoping it goes away.
  const [two, one] = await Promise.all([
    waitFor(TAB2, '[data-peers="1"]', 90_000),
    waitFor(TAB1, '[data-peers="1"]', 90_000),
  ])
  assert(two, 'tab 2 never saw tab 1')
  assert(one, 'tab 1 never saw tab 2')
})

await test('a message typed in tab 1 arrives in tab 2', async () => {
  const tries = await sayUntilSeen(TAB1, TAB2, 'hello from tab one')
  assert(tries > 0, 'tab 2 never got it')
  if (tries > 1) console.log(`        (took ${tries} attempts)`)
})

await test('a message typed in tab 2 arrives in tab 1', async () => {
  const tries = await sayUntilSeen(TAB2, TAB1, 'and hello back')
  assert(tries > 0, 'tab 1 never got it')
  if (tries > 1) console.log(`        (took ${tries} attempts)`)
})

// ---------------------------------------------------------------- summary

const failed = results.filter((r) => !r.ok)
const pend = results.filter((r) => r.pending)
const passed = results.filter((r) => r.ok && !r.pending)

console.log(
  `\n${passed.length} passed, ${failed.length} failed, ${pend.length} pending\n`,
)

if (failed.length > 0) process.exit(1)
