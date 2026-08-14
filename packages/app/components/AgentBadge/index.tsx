import { Badge } from 'moonspace-dom'
import { component, disposable, interval, signal } from 'visage-dom'

import { subscribeAgentActivity, type AgentActivity } from '../../lib/agentTools/index.ts'

/** How long after a call the page still claims the agent is here. */
const ACTIVE_MS = 15_000

export interface Described {
  label: string
  tone: 'accent' | 'neutral'
  title: string
}

/**
 * The wording is careful, because **the thing you would want to show cannot be
 * observed**. WebMCP lets a page publish tools; it never tells the page that
 * something connected to them, and the spec has no notion of an agent session.
 * A tab whose tools nobody has called is indistinguishable from a tab no agent
 * has found.
 *
 * So this is built from the only real evidence — a tool actually being invoked
 * — and never claims more. It is absent until the first call, and once the
 * active window lapses it drops the present tense. Do not "improve" it into a
 * connected/disconnected indicator; there is nothing to drive one with.
 *
 * Exported as a plain function so it can be tested without a DOM.
 */
export function describeAgent(activity: AgentActivity, now: number): Described | undefined {
  const { calls, running } = activity

  const last = calls.at(-1)
  if (!last) return undefined
  const when = last.settledAt ?? last.startedAt
  const detail = `${calls.length} tool call${calls.length === 1 ? '' : 's'}, last ${last.name}`

  if (running) return { label: '‹AGENT CONTROLLING›', tone: 'accent', title: detail }
  if (now - when < ACTIVE_MS) return { label: '‹AGENT ACTIVE›', tone: 'accent', title: detail }

  return {
    label: `‹AGENT · ${calls.length}›`,
    tone: 'neutral',
    title: `${detail} — it may no longer be attached`,
  }
}

export const AgentBadge = component(function* () {
  const activity = signal<AgentActivity>({ calls: [], running: false })
  const now = signal(Date.now())

  using _updates = disposable(subscribeAgentActivity((next) => (activity.value = next)))

  // The badge decays with time, not just with events, so it needs a tick — but
  // only once there is something to decay. Started unconditionally it wakes the
  // renderer 86,400 times a day to produce `null`, since no browser publishes
  // WebMCP by default and `calls` is empty on almost every tab.
  let clock: Disposable | undefined
  using _clock = disposable(() => clock?.[Symbol.dispose]())
  using _arm = disposable(
    subscribeAgentActivity((next) => {
      if (next.calls.length > 0 && !clock) clock = interval(1000, () => (now.value = Date.now()))
    }),
  )

  yield () => {
    const described = describeAgent(activity.value, now.value)
    if (!described) return null
    return (
      <Badge
        tone={described.tone}
        variant={described.tone === 'accent' ? 'solid' : 'outline'}
        title={described.title}
        data-agent-badge={described.tone}
      >
        {described.label}
      </Badge>
    )
  }
})
