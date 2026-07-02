import { state } from "../state";
import type { DelegationMode } from "../types";
import { injectSilent, notify } from "../ui";

// Tracks in-flight handover/task delegations. Two backends, chosen per call:
//
//   1. A todo plugin, if the user has one. A pi todo plugin exposes an
//      agent-facing tool (e.g. `todo`) — not an extension API — so "use it"
//      means the same delegation the Claude Code plugin uses: instruct the agent
//      to keep the item in its todo list, both when the delegation starts and on
//      every transition. We add no transcript line; the todo holds the status.
//   2. No todo plugin: print a simple line on start and on each state change.
//
// Detection is generic (any todo-like tool), never hardcoded to a package, and
// re-checked per call so a plugin installed mid-session is picked up without a
// reload (getAllTools reads an in-memory registry — no subprocess).

type Role = "initiator" | "receiver";

function detectTodoTool(): string | null {
  try {
    const tools = state.pi?.getAllTools() ?? [];
    // Match `todo`/`todos` as a whole name segment: catches `todo`, `add_todo`,
    // `todo_write`, but not unrelated tools like `todoist_search`.
    return tools.find((tool) => /(^|[_-])todos?([_-]|$)/i.test(tool.name))?.name ?? null;
  } catch {
    return null;
  }
}

// The initiator sent the task out (→ peer); the receiver took one in (from
// peer). Keeps the direction unambiguous on both sides.
function connector(role: Role, peer: string): string {
  return role === "receiver" ? `from \`<${peer}>\`` : `→ \`<${peer}>\``;
}

// Begin tracking a delegation this node is a party to.
export function trackStart(opts: {
  mode: DelegationMode;
  peer: string;
  role: Role;
  task?: string;
  status?: string;
}): void {
  const { mode, peer, role, task, status = "offered" } = opts;
  const todo = detectTodoTool();
  if (todo) {
    injectSilent(
      `Track this ${mode} ${connector(role, peer)}${task ? ` ("${task}")` : ""} in your todo list with the ${todo} tool, and keep its status current as it progresses (offered → accepted/declined → done → confirmed).`,
    );
    return;
  }
  notify(`${mode} ${connector(role, peer)}: ${status}`);
}

// Report a later state change. With a todo plugin the agent owns the running
// status, so we feed it the transition as context (no transcript line) rather
// than printing — keeping its todo list in sync.
export function trackStatus(opts: {
  mode: DelegationMode;
  peer: string;
  role: Role;
  status: string;
}): void {
  const { mode, peer, role, status } = opts;
  const todo = detectTodoTool();
  if (todo) {
    injectSilent(`Update your todo for the ${mode} ${connector(role, peer)}: now ${status}.`);
    return;
  }
  notify(`${mode} ${connector(role, peer)}: ${status}`);
}
