import { afterEach, expect, test } from 'bun:test'

import { agentActivity, formatArgs, registerAgentTools, TOOLS, unregisterAgentTools } from './index.ts'
import { resetActivity } from './activity.ts'
import { publishSession, resetSession, type GossipSession } from './session.ts'

afterEach(() => {
  unregisterAgentTools()
  resetSession()
  resetActivity()
  Reflect.deleteProperty(document, 'modelContext')
})

const byName = (name: string): ModelContextTool => {
  const tool = TOOLS.find((candidate) => candidate.name === name)
  if (!tool) throw new Error(`no such tool: ${name}`)
  return tool
}

/**
 * The names the CLI's stdio MCP server publishes
 * (`crates/agent-gossip/src/mcp/mod.rs`). An agent that knows one must know the
 * other, so this list is a contract rather than a convenience.
 */
const CLI_TOOLS = [
  'create_gossip',
  'join_gossip',
  'topic_gossip',
  'discover_gossips',
  'leave_gossip',
  'send_broadcast',
  'send_msg',
  'fetch_messages',
  'gossip_info',
  'ping',
  'gossip_version',
  'gossip_manual',
  'task_status',
  'task_artifact',
  'a2a_call',
  'apply_state_merge',
  'get_state',
  'apply_meta_merge',
  'get_meta',
]

test('the published tools match the CLI MCP server, name for name', () => {
  expect([...TOOLS.map((tool) => tool.name)].sort()).toEqual([...CLI_TOOLS].sort())
  expect(TOOLS).toHaveLength(19)
})

test('every tool has a description and a schema', () => {
  for (const tool of TOOLS) {
    expect(tool.description.length).toBeGreaterThan(10)
    expect(tool.inputSchema).toBeDefined()
    // 1–128 chars of ASCII alphanumeric, `_`, `-` or `.`, per the IDL.
    expect(tool.name).toMatch(/^[A-Za-z0-9_.-]{1,128}$/)
  }
})

test('a browser without WebMCP registers nothing and does not throw', async () => {
  const result = await registerAgentTools()
  expect(result).toEqual({ registered: false, names: [] })
})

test('registration publishes every tool and records them', async () => {
  const registered: string[] = []
  Object.defineProperty(document, 'modelContext', {
    configurable: true,
    value: {
      registerTool: async (tool: ModelContextTool) => {
        registered.push(tool.name)
      },
    },
  })

  const result = await registerAgentTools()
  expect(result.registered).toBe(true)
  expect(result.names).toHaveLength(19)
  expect(registered.sort()).toEqual([...CLI_TOOLS].sort())
})

test('one failing registration does not cost the others', async () => {
  Object.defineProperty(document, 'modelContext', {
    configurable: true,
    value: {
      registerTool: async (tool: ModelContextTool) => {
        if (tool.name === 'ping') throw new Error('InvalidStateError')
      },
    },
  })

  const result = await registerAgentTools()
  expect(result.names).toHaveLength(18)
  expect(result.names).not.toContain('ping')
})

// ------------------------------------------------------------------ behaviour

test('a tool needing a gossip fails as data, never by throwing', async () => {
  // A throw would reach the agent as "the invocation failed" with the message
  // stripped, which is indistinguishable from every other failure.
  const result = (await byName('send_broadcast').execute({ text: 'hi' })) as {
    ok: boolean
    code: string
  }
  expect(result.ok).toBe(false)
  expect(result.code).toBe('no_session')
})

test('arguments are validated by the tool, since the browser does not', async () => {
  // Chrome does not check input against inputSchema: a call omitting a required
  // field arrives with it undefined.
  const result = (await byName('send_msg').execute({ to: 'bob' })) as { ok: boolean; code: string }
  expect(result.ok).toBe(false)
  expect(result.code).toBe('bad_argument')
  expect((result as unknown as { error: string }).error).toContain('text')
})

test('tools that cannot mean the same thing in a tab say so', async () => {
  for (const name of ['create_gossip', 'join_gossip', 'topic_gossip', 'discover_gossips']) {
    const result = (await byName(name).execute({ gossip: 'x', string: 'x' })) as { code: string }
    expect(result.code).toBe('unavailable')
  }
  const manual = (await byName('gossip_manual').execute({})) as { code: string }
  expect(manual.code).toBe('unsupported')
})

test('gossip_version answers without a gossip', async () => {
  const result = (await byName('gossip_version').execute({})) as { ok: boolean; runtime: string }
  expect(result.ok).toBe(true)
  expect(result.runtime).toBe('browser')
})

test('a joined session makes the gossip tools work', async () => {
  publishSession({
    mesh: 'MESH',
    name: 'room',
    nickname: 'tab',
    transport: 'webrtc',
    peers: () => [{ nickname: 'cli' }],
    broadcast: async () => ({ id: 'm1' }),
  } as unknown as GossipSession)

  const info = (await byName('gossip_info').execute({})) as {
    ok: boolean
    peer_count: number
    transport: string
  }
  expect(info.ok).toBe(true)
  expect(info.peer_count).toBe(1)
  expect(info.transport).toBe('webrtc')

  const sent = (await byName('send_broadcast').execute({ text: 'hello' })) as {
    ok: boolean
    id: string
  }
  expect(sent).toEqual({ ok: true, id: 'm1' })
})

test('a throw from the client becomes a failure result', async () => {
  publishSession({
    broadcast: async () => {
      throw new Error('transport closed')
    },
  } as unknown as GossipSession)

  const result = (await byName('send_broadcast').execute({ text: 'hi' })) as {
    ok: boolean
    code: string
    error: string
  }
  expect(result.ok).toBe(false)
  expect(result.code).toBe('failed')
  expect(result.error).toBe('transport closed')
})

// ------------------------------------------------------------------ the log

test('a nested password is never written into the call log', () => {
  // The merge tools take an arbitrary object, so redaction has to hold at depth
  // and not just on the top-level key.
  const nested = formatArgs({ merge: { creds: { password: 'hunter2' } } })
  expect(nested).not.toContain('hunter2')
  expect(nested).toContain('***')
})

test('a large argument is labelled, not serialized', () => {
  // Stringifying a million-element array to keep 120 characters would stall the
  // main thread on every tool call.
  const rendered = formatArgs({ items: Array.from({ length: 10_000 }, (_, i) => i) })
  expect(rendered).toBe('items: [10000 items]')
})

test('a password is never written into the call log', () => {
  // The log is drawn on the page, so a plain JSON.stringify of the arguments
  // would print the mesh password in front of whoever is watching the tab.
  const rendered = formatArgs({ gossip: 'MESH', password: 'hunter2', nickname: 'tab' })
  expect(rendered).not.toContain('hunter2')
  expect(rendered).toContain('password: ***')
  expect(rendered).toContain('gossip: MESH')
})

test('every redacted key is covered whatever its case', () => {
  for (const key of ['password', 'Password', 'SECRET', 'token', 'passphrase']) {
    expect(formatArgs({ [key]: 'sensitive' })).not.toContain('sensitive')
  }
})

test('a long argument is truncated rather than flooding the log', () => {
  const rendered = formatArgs({ text: 'x'.repeat(1000) })
  expect(rendered.length).toBeLessThan(200)
  expect(rendered).toContain('…')
})

test('calls are logged with their outcome', async () => {
  // Registration wraps each tool, so drive the wrapped copy the way the browser
  // would rather than the bare export.
  const wrapped: ModelContextTool[] = []
  Object.defineProperty(document, 'modelContext', {
    configurable: true,
    value: {
      registerTool: async (tool: ModelContextTool) => {
        wrapped.push(tool)
      },
    },
  })
  await registerAgentTools()

  const broadcast = wrapped.find((tool) => tool.name === 'send_broadcast')
  await broadcast?.execute({ text: 'hi', password: 'hunter2' })

  const { calls, running } = agentActivity()
  expect(calls).toHaveLength(1)
  expect(calls[0]?.name).toBe('send_broadcast')
  expect(calls[0]?.ok).toBe(false)
  expect(calls[0]?.code).toBe('no_session')
  expect(calls[0]?.settledAt).toBeDefined()
  expect(calls[0]?.args).not.toContain('hunter2')
  // The badge must not be left stuck on after a call settles.
  expect(running).toBe(false)
})
