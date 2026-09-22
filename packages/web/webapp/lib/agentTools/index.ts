/**
 * Publish this page's tools to an agent, using WebMCP.
 *
 * WebMCP (`document.modelContext`) is a W3C Community Group draft. No browser
 * enables it by default, so this is a no-op almost everywhere and must stay
 * cheap enough not to care — one property read, then nothing.
 *
 * There is deliberately no polyfill. The way an agent reaches these tools today
 * is Chrome's own `chrome-devtools-mcp`, which launches the browser and can
 * therefore turn the native feature on itself. A polyfill would add a
 * dependency to serve a case that does not exist: a browser without WebMCP is
 * also a browser with nothing on the other end to call it.
 */

import { beginToolCall } from './activity.ts'
import type { ToolResult } from './result.ts'
import { TOOLS } from './tools.ts'

export {
  agentActivity,
  subscribeAgentActivity,
  formatArgs,
  type AgentActivity,
  type AgentCall,
} from './activity.ts'
export { publishSession, type GossipSession } from './session.ts'
export { TOOLS } from './tools.ts'

/**
 * Wrap a tool so its invocations are visible to the page.
 *
 * Nothing tells a page that an agent has connected — WebMCP has no such signal
 * — so being *called* is the only evidence there is, and the badge and the call
 * log are built entirely out of it. Wrapping here rather than in each tool
 * means a tool cannot be added and quietly left out of the log.
 */
function instrument(tool: ModelContextTool): ModelContextTool {
  return {
    ...tool,
    execute: async (input) => {
      const end = beginToolCall(tool.name, input ?? {})
      // Left undefined by a throw, which is how `end` tells a failure that came
      // back as a result from one that escaped the tool's own `guard`.
      let result: ToolResult<object> | undefined
      try {
        result = (await tool.execute(input)) as ToolResult<object>
        return result
      } finally {
        end(
          result === undefined
            ? undefined
            : { ok: result.ok, ...(result.ok ? {} : { code: result.code }) },
        )
      }
    },
  }
}

/**
 * Guards against a second registration in one document.
 *
 * `registerTool` rejects a duplicate name with `InvalidStateError`, and the dev
 * server's hot reload re-runs the entry module against a document that still
 * holds the previous registration. Aborting the old batch first makes a reload
 * replace the tools rather than fail on the first one.
 */
let current: AbortController | undefined

export interface RegisterResult {
  /** False when this browser has no WebMCP, which is the common case. */
  registered: boolean
  names: string[]
}

export async function registerAgentTools(): Promise<RegisterResult> {
  const modelContext = document.modelContext
  if (!modelContext) return { registered: false, names: [] }

  current?.abort()
  const controller = new AbortController()
  current = controller

  // Registered together rather than one after another. Each `registerTool` is a
  // round trip, and until the last one lands an agent calling `getTools()` sees
  // a partial set — it would not be told the list is still filling, so it would
  // simply conclude the missing tools do not exist. agent-share measured a
  // sequential batch returning three of eight.
  const settled = await Promise.all(
    TOOLS.map(async (tool) => {
      try {
        await modelContext.registerTool(instrument(tool), { signal: controller.signal })
        return tool.name
      } catch (error) {
        // One bad tool must not cost the others. A name collision with
        // something else on the page is the likely cause, and it is worth
        // saying out loud.
        console.warn(`[agent-gossip] could not publish the "${tool.name}" tool`, error)
        return undefined
      }
    }),
  )

  const names = settled.filter((name): name is string => name !== undefined)
  return { registered: names.length > 0, names }
}

/** Withdraw the tools. Aborting the signal is the only way to unregister. */
export function unregisterAgentTools(): void {
  current?.abort()
  current = undefined
}
