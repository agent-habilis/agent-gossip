/**
 * The tools this page publishes, tracking the CLI's stdio MCP server
 * (`crates/agent-gossip/src/mcp/mod.rs`) name for name and schema for schema.
 *
 * That correspondence is the point: an agent that knows `agent-gossip mcp`
 * knows this tab, and a skill written against one drives the other. Where a
 * tool cannot mean the same thing in a browser it is refused with a reason
 * rather than quietly given different semantics — see `create_gossip`'s mdns
 * and dht arguments, which do not exist in a tab.
 */

import {
  fail,
  guard,
  ok,
  optionalInt,
  optionalString,
  requiredObject,
  requiredString,
  type ToolResult,
} from './result.ts'
import { activeSession, type GossipSession } from './session.ts'

const str = (description: string) => ({ type: 'string', description })

function object(
  properties: Record<string, unknown>,
  required: string[] = [],
): Record<string, unknown> {
  return { type: 'object', properties, ...(required.length > 0 ? { required } : {}) }
}

/**
 * Parse the arguments, *then* look for a gossip, then run.
 *
 * The order is the point. Checking the session first would report `no_session`
 * for a call that is also malformed, and the agent would go off joining a
 * gossip when the actual problem was a missing field. A bad argument is
 * deterministic and the agent can fix it; whether a tab is in a gossip is not
 * its fault.
 */
function withSession<A, T extends object>(
  parse: (input: Record<string, unknown>) => A,
  run: (gossip: GossipSession, args: A) => Promise<T>,
): (input: Record<string, unknown>) => Promise<ToolResult<T>> {
  return (input) =>
    guard(async () => {
      const args = parse(input ?? {})
      const gossip = activeSession()
      if (!gossip) return fail('no_session', 'not in a gossip — open a room first')
      return ok(await run(gossip, args))
    })
}

const NO_PATH = 'the browser client has no path to this: a tab has no mDNS, no DHT and no directory'

/**
 * `Bun.build`'s `define` is a textual substitution, so the identifier exists
 * only in a built bundle — under `bun test` it is simply undeclared. The
 * `typeof` guard survives substitution (it becomes `typeof "0.8.0"`), which a
 * bare `??` would not.
 */
const VERSION = typeof __APP_VERSION__ === 'undefined' ? 'dev' : __APP_VERSION__

export const TOOLS: readonly ModelContextTool[] = [
  // ---------------------------------------------------------------- lifecycle
  {
    name: 'create_gossip',
    description:
      'Create a new gossip and join it in this tab. The tab navigates to the new gossip URL, which is the same URL anyone else uses to join. Note a browser has no mDNS and no DHT, so a gossip created here is reachable through the relay only.',
    inputSchema: object({
      name: str('Display name for the gossip.'),
      nickname: str('This peer’s nickname. Generated when omitted.'),
      password: str('Protect the gossip; joiners must present this.'),
    }),
    execute: (input) =>
      guard(async () => {
        optionalString(input, 'name')
        optionalString(input, 'nickname')
        return fail('unavailable', NO_PATH)
      }),
  },
  {
    name: 'join_gossip',
    description:
      'Join an existing gossip by id in this tab. The tab navigates to that gossip’s URL.',
    inputSchema: object(
      {
        gossip: str('The gossip id, or a full agent-gossip.com URL containing one.'),
        nickname: str('This peer’s nickname. Generated when omitted.'),
        password: str('Required when the gossip is protected.'),
      },
      ['gossip'],
    ),
    execute: (input) =>
      guard(async () => {
        requiredString(input, 'gossip')
        return fail('unavailable', NO_PATH)
      }),
  },
  {
    name: 'topic_gossip',
    description:
      'Join the public gossip derived from a shared string. Everyone who derives from the same string lands in the same gossip, with no id to exchange.',
    inputSchema: object({ string: str('The shared string.') }, ['string']),
    execute: (input) =>
      guard(async () => {
        requiredString(input, 'string')
        return fail('unavailable', NO_PATH)
      }),
  },
  {
    name: 'leave_gossip',
    description: 'Leave the gossip this tab is in.',
    inputSchema: object({}),
    execute: withSession(
      () => undefined,
      async (gossip) => (await gossip.leave(), {}),
    ),
  },
  {
    name: 'discover_gossips',
    description: 'List gossips advertised in a directory.',
    inputSchema: object({ directory: str('Directory to query. The default when omitted.') }),
    execute: (input) =>
      guard(async () => {
        optionalString(input, 'directory')
        return fail('unavailable', NO_PATH)
      }),
  },

  // ------------------------------------------------------------------ talking
  {
    name: 'send_broadcast',
    description: 'Send a message to everyone in the gossip.',
    inputSchema: object({ text: str('The message body.') }, ['text']),
    execute: withSession(
      (input) => requiredString(input, 'text'),
      async (gossip, text) => gossip.broadcast(text),
    ),
  },
  {
    name: 'send_msg',
    description:
      'Send a sealed message to one peer. Unlike a broadcast, no other peer can read it.',
    inputSchema: object({ to: str('Recipient nickname.'), text: str('The message body.') }, [
      'to',
      'text',
    ]),
    execute: withSession(
      (input) => ({ to: requiredString(input, 'to'), text: requiredString(input, 'text') }),
      async (gossip, { to, text }) => gossip.msg(to, text),
    ),
  },
  {
    name: 'fetch_messages',
    description:
      'Fetch messages received since a sequence number. Poll this rather than expecting messages to be pushed. Agent tool calls are never returned here — they are page state, not mesh traffic.',
    // No `long`: the CLI can block on a poll, this cannot, and advertising the
    // flag without honouring it makes an empty page look like an empty gossip.
    inputSchema: object({
      after: { type: 'integer', description: 'Return messages after this sequence number.' },
    }),
    annotations: { readOnlyHint: true, untrustedContentHint: true },
    execute: withSession(
      (input) => optionalInt(input, 'after', { min: 0, max: 2 ** 53 - 1, fallback: 0 }),
      async (gossip, after) => gossip.messages(after),
    ),
  },

  // ------------------------------------------------------------------ reading
  {
    name: 'gossip_info',
    description: 'The gossip this tab is in: id, name, nickname, and the peer roster.',
    inputSchema: object({}),
    annotations: { readOnlyHint: true },
    execute: withSession(
      () => undefined,
      async (gossip) => {
        const peers = gossip.peers()
        return {
          gossip: gossip.mesh,
          name: gossip.name,
          nickname: gossip.nickname,
          transport: gossip.transport,
          peer_count: peers.length,
          peers,
        }
      },
    ),
  },
  {
    name: 'ping',
    description: 'Round-trip time to each peer, to check liveness.',
    inputSchema: object({}),
    annotations: { readOnlyHint: true },
    execute: withSession(
      () => undefined,
      async (gossip) => ({ peers: await gossip.ping() }),
    ),
  },
  {
    name: 'gossip_version',
    description: 'The version of this agent-gossip build.',
    inputSchema: object({}),
    annotations: { readOnlyHint: true },
    execute: () => Promise.resolve(ok({ version: VERSION, runtime: 'browser' })),
  },
  {
    name: 'gossip_manual',
    description: 'The agent-gossip manual.',
    inputSchema: object({}),
    annotations: { readOnlyHint: true },
    execute: () =>
      Promise.resolve(
        fail(
          'unsupported',
          'the manual is not bundled into the browser client; run `agent-gossip man`, or read https://agent-gossip.com/',
        ),
      ),
  },

  // ------------------------------------------------------- shared documents
  {
    name: 'get_state',
    description: 'Read the gossip’s shared state document.',
    inputSchema: object({}),
    annotations: { readOnlyHint: true, untrustedContentHint: true },
    execute: withSession(
      () => undefined,
      async (gossip) => ({ state: await gossip.getState() }),
    ),
  },
  {
    name: 'apply_state_merge',
    description: 'Apply an RFC 7386 merge patch to the shared state document.',
    inputSchema: object({ merge: { type: 'object', description: 'The merge patch.' } }, ['merge']),
    execute: withSession(
      (input) => requiredObject(input, 'merge'),
      async (gossip, merge) => ({ state: await gossip.mergeState(merge) }),
    ),
  },
  {
    name: 'get_meta',
    description: 'Read the gossip’s metadata document, which carries each peer’s agent card.',
    inputSchema: object({}),
    annotations: { readOnlyHint: true, untrustedContentHint: true },
    execute: withSession(
      () => undefined,
      async (gossip) => ({ meta: await gossip.getMeta() }),
    ),
  },
  {
    name: 'apply_meta_merge',
    description: 'Apply an RFC 7386 merge patch to the metadata document.',
    inputSchema: object({ merge: { type: 'object', description: 'The merge patch.' } }, ['merge']),
    execute: withSession(
      (input) => requiredObject(input, 'merge'),
      async (gossip, merge) => ({ meta: await gossip.mergeMeta(merge) }),
    ),
  },

  // -------------------------------------------------------------------- tasks
  {
    name: 'task_status',
    description: 'Report progress on a task delegated to this peer.',
    inputSchema: object(
      {
        task_id: str('The task id.'),
        state: str('working, input-required, completed, failed, or canceled.'),
        note: str('A short human-readable note.'),
      },
      ['task_id', 'state'],
    ),
    execute: withSession(
      (input) => ({
        taskId: requiredString(input, 'task_id'),
        state: requiredString(input, 'state'),
        note: optionalString(input, 'note'),
      }),
      async (gossip, { taskId, state, note }) => ({
        result: await gossip.taskStatus(taskId, state, note),
      }),
    ),
  },
  {
    name: 'task_artifact',
    description: 'Attach a result to a task.',
    inputSchema: object({ task_id: str('The task id.'), text: str('The artifact body.') }, [
      'task_id',
      'text',
    ]),
    execute: withSession(
      (input) => ({
        taskId: requiredString(input, 'task_id'),
        text: requiredString(input, 'text'),
      }),
      async (gossip, { taskId, text }) => ({
        result: await gossip.taskArtifact(taskId, text),
      }),
    ),
  },
  {
    name: 'a2a_call',
    description: 'Make an A2A JSON-RPC call to one peer over the gossip.',
    inputSchema: object(
      {
        to: str('Peer nickname.'),
        method: str('The A2A method.'),
        params: { type: 'object', description: 'Method parameters.' },
        timeout_secs: { type: 'integer', description: 'Seconds to wait. Default 30.' },
      },
      ['to', 'method'],
    ),
    execute: withSession(
      (input) => ({
        to: requiredString(input, 'to'),
        method: requiredString(input, 'method'),
        params: input['params'] ?? {},
        timeout: optionalInt(input, 'timeout_secs', { min: 1, max: 300, fallback: 30 }),
      }),
      async (gossip, { to, method, params, timeout }) => ({
        result: await gossip.a2aCall(to, method, params, timeout),
      }),
    ),
  },
]
