import { beforeEach, expect, test } from "bun:test";
import { state } from "../state";
import type { SwarmEvent } from "../types";
import { engagementKind, formatDisplay, formatMessage, formatOutbound, formatState } from "./index";

const ev = (over: Partial<SwarmEvent>): SwarmEvent => ({ event: "message", type: "msg", ...over });

beforeEach(() => {
  // formatMessage suppresses pongs while a ping we sent is still outstanding.
  state.pingPending = false;
});

test("engagementKind: a reply addressed to us is directed", () => {
  expect(engagementKind(ev({ author: "a", body: "hi", reply: "me" }), "me")).toBe("directed");
});

test("engagementKind: a message with no reply is a broadcast", () => {
  expect(engagementKind(ev({ author: "a", body: "anyone?", reply: null }), "me")).toBe("broadcast");
});

test("engagementKind: a reply aimed at another peer does not engage us", () => {
  expect(engagementKind(ev({ author: "a", body: "hi", reply: "bob" }), "me")).toBeNull();
});

test("engagementKind: self, ping, pong, and non-messages never engage", () => {
  expect(engagementKind(ev({ author: "a", body: "hi", reply: null, self: true }), "me")).toBeNull();
  expect(engagementKind(ev({ author: "a", body: "ping", reply: null }), "me")).toBeNull();
  expect(engagementKind(ev({ author: "a", body: "pong", reply: null }), "me")).toBeNull();
  expect(engagementKind(ev({ author: "a", reply: null }), "me")).toBeNull(); // no body
  expect(engagementKind(ev({ event: "presence", type: "presence", author: "a" }), "me")).toBeNull();
});

test("engagementKind: directed requires our nick, not just any reply", () => {
  expect(engagementKind(ev({ author: "a", body: "hi", reply: "me" }), undefined)).toBeNull();
});

test("formatMessage wraps author and addressee as code spans", () => {
  expect(formatMessage(ev({ author: "ada", body: "hi", reply: null }))).toBe("`<ada>`: hi");
  expect(formatMessage(ev({ author: "ada", body: "hi", reply: "bob" }))).toBe(
    "`<ada>` → `<bob>`: hi",
  );
});

test("formatMessage handles ping and pong (and suppresses pong while pinging)", () => {
  expect(formatMessage(ev({ author: "ada", body: "ping", reply: null }))).toBe("ping → pong");
  expect(formatMessage(ev({ author: "ada", body: "pong" }))).toBe("pong from `<ada>`");
  state.pingPending = true;
  expect(formatMessage(ev({ author: "ada", body: "pong" }))).toBeNull();
});

test("formatOutbound echoes our own sent line as a code span (no bee — notify adds it)", () => {
  expect(formatOutbound("me", "hello all")).toBe("`<me>`: hello all");
  expect(formatOutbound("me", "on it", "bob")).toBe("`<me>` → `<bob>`: on it");
});

test("formatDisplay drops self and info/error, renders presence and messages", () => {
  expect(formatDisplay(ev({ author: "a", body: "hi", self: true }))).toBeNull();
  expect(formatDisplay(ev({ event: "error", body: "x" }))).toBeNull();
  expect(
    formatDisplay(ev({ event: "presence", type: "presence", subtype: "joined", author: "ada" })),
  ).toBe("`<ada>` joined");
  expect(formatDisplay(ev({ author: "ada", body: "hi", reply: null }))).toBe("`<ada>`: hi");
});

test("engagementKind: a peer state change engages as state; our own does not", () => {
  expect(
    engagementKind(ev({ event: "state", type: "state", author: "ada", self: false }), "me"),
  ).toBe("state");
  expect(
    engagementKind(ev({ event: "state", type: "state", author: "me", self: true }), "me"),
  ).toBeNull();
});

test("formatState renders a peer state change; formatDisplay drops our own", () => {
  expect(formatState(ev({ event: "state", type: "state", author: "ada" }))).toBe(
    "`<ada>` changed shared state",
  );
  expect(formatDisplay(ev({ event: "state", type: "state", author: "ada", self: false }))).toBe(
    "`<ada>` changed shared state",
  );
  expect(formatDisplay(ev({ event: "state", type: "state", author: "me", self: true }))).toBeNull();
});
