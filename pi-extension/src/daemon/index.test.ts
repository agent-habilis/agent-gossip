import { beforeEach, expect, test } from "bun:test";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { state } from "../state";
import { flushMessageBatch, parseOfferMarker, processDaemonLine } from "./index";

// The delegation flavor rides in-band as a leading [[handover]]/[[task]] marker
// on the offer body (the wire dropped its `kind` discriminator). parseOfferMarker
// is the only place that decision is recovered, so its tolerance is load-bearing:
// a miss silently downgrades a handover to a report-back task and leaks the raw
// marker into the agent's brief.
test("parseOfferMarker reads the marker and strips it from the brief", () => {
  expect(parseOfferMarker("[[handover]]\nreview src/net")).toEqual({
    mode: "handover",
    body: "review src/net",
  });
  expect(parseOfferMarker("[[task]]\nreview src/net")).toEqual({
    mode: "task",
    body: "review src/net",
  });
});

test("parseOfferMarker tolerates leading blank lines and whitespace", () => {
  // LLM-authored briefs routinely prepend a blank line; the marker must still
  // be recognized rather than leaking through as literal text.
  expect(parseOfferMarker("\n\n[[handover]]\nbrief")).toEqual({
    mode: "handover",
    body: "brief",
  });
  expect(parseOfferMarker("  \t[[task]]  \nbrief")).toEqual({ mode: "task", body: "brief" });
});

test("parseOfferMarker defaults an unmarked body to task and leaves it intact", () => {
  expect(parseOfferMarker("just a plain brief")).toEqual({
    mode: "task",
    body: "just a plain brief",
  });
  expect(parseOfferMarker(undefined)).toEqual({ mode: "task", body: "" });
});

// The contract this locks: a message directed at us and a broadcast both wake
// the agent (an inject, which triggers a turn); a reply aimed at another peer
// and presence lines are pure prints with no turn. A past refactor silently
// flipped the directed path to a no-turn print — types and lint stayed green,
// so only an executing test guards it.

type RecordedSend = {
  customType: string;
  display: boolean;
  content: string;
  options: Record<string, unknown>;
};
const sends: RecordedSend[] = [];

beforeEach(() => {
  sends.length = 0;
  state.session = { swarm: "🐝-test", name: "test", nickname: "me" };
  state.ctx = { isIdle: () => true } as unknown as ExtensionContext;
  state.pi = {
    sendMessage: (
      message: { customType: string; display: boolean; content?: string },
      options: Record<string, unknown> = {},
    ) => {
      sends.push({
        customType: message.customType,
        display: message.display,
        content: message.content ?? "",
        options,
      });
    },
  } as unknown as ExtensionAPI;
  state.messageBatch = [];
  if (state.messageBatchTimer) clearTimeout(state.messageBatchTimer);
  state.messageBatchTimer = null;
  state.pendingMessages = [];
  // The routing tests below assert a single send; the standing policy is a
  // one-time concern covered by its own test, so mark it already delivered.
  state.policySent = true;
});

const feed = (event: Record<string, unknown>) => processDaemonLine(JSON.stringify(event));

test("a message directed to us wakes the agent", () => {
  feed({ event: "message", type: "msg", author: "ada", body: "you around?", reply: "me" });
  flushMessageBatch();
  expect(sends).toHaveLength(1);
  expect(sends[0]?.customType).toBe("swarm-inject");
  expect(sends[0]?.options.triggerTurn).toBe(true);
});

test("a broadcast wakes the agent", () => {
  feed({ event: "message", type: "msg", author: "ada", body: "anyone know x?", reply: null });
  flushMessageBatch();
  expect(sends).toHaveLength(1);
  expect(sends[0]?.customType).toBe("swarm-inject");
  expect(sends[0]?.options.triggerTurn).toBe(true);
});

test("a reply aimed at another peer is a pure print, no turn", () => {
  feed({ event: "message", type: "msg", author: "ada", body: "hi", reply: "bob" });
  flushMessageBatch();
  expect(sends).toHaveLength(1);
  expect(sends[0]?.customType).toBe("swarm-info");
  expect(sends[0]?.options.triggerTurn).toBeUndefined();
  expect(sends[0]?.options.deliverAs).toBeUndefined();
});

test("a presence line is a pure print, no turn", () => {
  feed({ event: "presence", type: "presence", subtype: "joined", author: "ada" });
  flushMessageBatch();
  expect(sends).toHaveLength(1);
  expect(sends[0]?.customType).toBe("swarm-info");
  expect(sends[0]?.options.triggerTurn).toBeUndefined();
});

test("a peer's state change wakes the agent with the document", () => {
  feed({
    event: "state",
    type: "state",
    author: "ada",
    self: false,
    merge: { turn: "me" },
    document: { turn: "me", n: 1 },
  });
  flushMessageBatch();
  expect(sends).toHaveLength(1);
  expect(sends[0]?.customType).toBe("swarm-inject");
  expect(sends[0]?.options.triggerTurn).toBe(true);
  // The derived document rides in the wake text so the agent reacts in one go.
  expect(sends[0]?.content).toContain('"turn": "me"');
  expect(sends[0]?.content).toContain("changed shared state");
});

test("our own state change neither wakes nor prints", () => {
  feed({
    event: "state",
    type: "state",
    author: "me",
    self: true,
    document: { turn: "ada", n: 2 },
  });
  flushMessageBatch();
  expect(sends).toHaveLength(0);
});

test("ping/pong never wakes the agent", () => {
  // Session nulled so the auto-pong path doesn't shell out to `ahsw` in a unit
  // test; we only assert ping is not treated as an engageable message.
  state.session = null;
  feed({ event: "message", type: "msg", author: "ada", body: "ping", reply: null });
  flushMessageBatch();
  expect(sends.every((send) => send.customType !== "swarm-inject")).toBe(true);
});

test("the reply policy is injected once, silently, then never again", () => {
  state.policySent = false;
  feed({ event: "message", type: "msg", author: "ada", body: "hi", reply: "me" });
  flushMessageBatch();

  const policy = sends.find((send) => send.customType === "swarm-context");
  expect(policy).toBeDefined();
  expect(policy?.display).toBe(false); // never rendered in the UI
  expect(policy?.options.triggerTurn).toBeUndefined(); // context, not an action
  // The peer message still wakes the agent, and it is displayed.
  const message = sends.find((send) => send.customType === "swarm-inject");
  expect(message?.display).toBe(true);
  expect(message?.options.triggerTurn).toBe(true);

  // A second batch does not re-inject the policy.
  sends.length = 0;
  feed({ event: "message", type: "msg", author: "ada", body: "again", reply: "me" });
  flushMessageBatch();
  expect(sends.some((send) => send.customType === "swarm-context")).toBe(false);
});
