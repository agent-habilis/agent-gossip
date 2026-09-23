/**
 * What the page knows about an agent driving it — which is only ever "a tool
 * was called", and never "an agent is connected".
 *
 * WebMCP gives a page no signal that something attached to its tools. There is
 * no session, no connect event, and `getTools()` is a *caller's* API: a page
 * calling it learns about its own tools, not about who is looking at them. A
 * tab whose tools nobody has called is indistinguishable from a tab no agent
 * has found. So everything here is built from invocations, and claims no more.
 */

export interface AgentCall {
  readonly name: string
  /** Arguments, already redacted for display. */
  readonly args: string
  readonly startedAt: number
  readonly settledAt?: number
  readonly ok?: boolean
  /** The stable failure code, when it failed. */
  readonly code?: string
}

export interface AgentActivity {
  readonly calls: readonly AgentCall[]
  /** A call is in flight. */
  readonly running: boolean
}

/**
 * Keys whose values must never be rendered. `join_gossip` and `create_gossip`
 * both take a mesh password, and the call log is drawn on the page — so a plain
 * `JSON.stringify` of the arguments would write it out in front of whoever is
 * watching, and into any screenshot of the tab.
 */
const REDACT = new Set(['password', 'secret', 'token', 'passphrase'])

const MAX_ARG_CHARS = 120
const MAX_CALLS = 200

let calls: AgentCall[] = []
let running = 0

type Listener = (activity: AgentActivity) => void
const listeners = new Set<Listener>()

function snapshot(): AgentActivity {
  return { calls, running: running > 0 }
}

function emit(): void {
  const next = snapshot()
  for (const listener of listeners) listener(next)
}

export function agentActivity(): AgentActivity {
  return snapshot()
}

export function subscribeAgentActivity(listener: Listener): () => void {
  listeners.add(listener)
  listener(snapshot())
  return () => listeners.delete(listener)
}

/**
 * A compact, redacted rendering of a call's arguments.
 *
 * Containers are labelled by size rather than serialized. The browser does not
 * validate input against `inputSchema`, so an argument can be a million-element
 * array — and this runs on the main thread for *every* tool call, so
 * stringifying one only to keep 120 characters is a stall for nothing.
 */
export function formatArgs(input: Record<string, unknown>): string {
  const entries = Object.entries(input ?? {})
  if (entries.length === 0) return ''
  return entries.map(([key, value]) => `${key}: ${describeValue(key, value)}`).join(' · ')
}

function describeValue(key: string, value: unknown): string {
  // Checked at every depth, not just the top: `apply_state_merge` takes an
  // arbitrary object, so `{ merge: { creds: { password: … } } }` would
  // otherwise be rendered whole onto the page.
  if (REDACT.has(key.toLowerCase())) return '***'
  if (value === null || value === undefined) return String(value)
  if (Array.isArray(value)) return `[${value.length} items]`
  if (typeof value === 'object') {
    const keys = Object.keys(value as object)
    // Small objects are worth showing, but only through this same filter.
    if (keys.length > 4) return `{${keys.length} keys}`
    return `{${keys.map((k) => `${k}: ${describeValue(k, (value as Record<string, unknown>)[k])}`).join(', ')}}`
  }
  const text = typeof value === 'string' ? value : String(value)
  return text.length > MAX_ARG_CHARS ? `${text.slice(0, MAX_ARG_CHARS)}…` : text
}

/**
 * Record the start of a call; the returned function records how it ended.
 *
 * `result` is `undefined` when the tool threw rather than returned — which
 * should not happen, since every tool is wrapped in `guard`, and is worth
 * showing differently when it does.
 */
export function beginToolCall(
  name: string,
  input: Record<string, unknown>,
): (result?: { ok: boolean; code?: string }) => void {
  const call: AgentCall = { name, args: formatArgs(input), startedAt: Date.now() }
  calls = [...calls, call].slice(-MAX_CALLS)
  running += 1
  emit()

  let done = false
  return (result) => {
    // Guard against a double settle: `finally` runs once, but a tool that
    // resolved twice would otherwise decrement `running` below zero and leave
    // the badge stuck on.
    if (done) return
    done = true

    running -= 1
    calls = calls.map((entry) =>
      entry === call
        ? {
            ...entry,
            settledAt: Date.now(),
            ok: result?.ok ?? false,
            ...(result?.code === undefined ? {} : { code: result.code }),
          }
        : entry,
    )
    emit()
  }
}

/** Test seam. Never called by the app. */
export function resetActivity(): void {
  calls = []
  running = 0
  emit()
}
