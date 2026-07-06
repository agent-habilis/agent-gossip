import { beforeEach, expect, test } from "bun:test";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { state } from "../state";
import { trackStart, trackStatus } from "./index";

type RecordedSend = { customType: string; display: boolean; content: string };
const sends: RecordedSend[] = [];

// Build a fake `pi` whose tool registry is whatever we pass, and which records
// every sendMessage so we can assert on the tracker's output. Detection runs
// per call, so just setting the tools here is enough.
function withTools(toolNames: string[]): void {
  sends.length = 0;
  state.ctx = null;
  state.pi = {
    getAllTools: () => toolNames.map((name) => ({ name })),
    sendMessage: (message: { customType: string; display: boolean; content?: string }) => {
      sends.push({
        customType: message.customType,
        display: message.display,
        content: message.content ?? "",
      });
    },
  } as unknown as ExtensionAPI;
}

beforeEach(() => {
  sends.length = 0;
});

test("no todo plugin: prints a simple line on start and on each state change", () => {
  withTools(["square_send", "square_task"]); // square_* must never count as a todo tool
  trackStart({ mode: "handover", peer: "bob", role: "initiator" });
  trackStatus({ mode: "handover", peer: "bob", role: "initiator", status: "accepted" });

  expect(sends).toHaveLength(2);
  expect(sends[0]?.customType).toBe("square-info");
  expect(sends[0]?.content).toBe("💬️ handover → `<bob>`: offered");
  expect(sends[1]?.content).toBe("💬️ handover → `<bob>`: accepted");
});

test("receiver side reads 'from <peer>', not '→'", () => {
  withTools([]);
  trackStart({ mode: "task", peer: "alice", role: "receiver", status: "accepted" });
  expect(sends[0]?.content).toBe("💬️ task from `<alice>`: accepted");
});

test("todo plugin present: nudges the agent silently on start AND each change", () => {
  withTools(["todo"]);
  trackStart({ mode: "task", peer: "ada", role: "initiator", task: "review src/net" });
  trackStatus({ mode: "task", peer: "ada", role: "initiator", status: "accepted" });

  // Both legs go to the agent as silent context (no transcript line), so the
  // todo stays in sync — never a no-op.
  expect(sends).toHaveLength(2);
  expect(sends.every((send) => send.customType === "square-context")).toBe(true);
  expect(sends.every((send) => send.display === false)).toBe(true);
  expect(sends[0]?.content).toContain("todo");
  expect(sends[1]?.content).toContain("accepted");
});

test("unrelated 'todo'-substring tools do not flip into todo mode", () => {
  withTools(["todoist_search"]); // contains 'todo' but isn't a todo list
  trackStatus({ mode: "task", peer: "ada", role: "initiator", status: "done" });
  // Falls back to a printed line rather than silently delegating.
  expect(sends).toHaveLength(1);
  expect(sends[0]?.customType).toBe("square-info");
});
