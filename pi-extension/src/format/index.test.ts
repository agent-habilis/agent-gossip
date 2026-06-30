import { expect, test } from "bun:test";
import type { SwarmEvent } from "../types";
import {
  engagementKind,
  formatDisplay,
  formatMessage,
  formatMeta,
  formatOutbound,
  formatPeerIdent,
  formatState,
} from "./index";

const ev = (over: Partial<SwarmEvent>): SwarmEvent => ({ event: "message", type: "msg", ...over });

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

test("formatMessage stays silent on ambient ping/pong (daemon handles them)", () => {
  expect(formatMessage(ev({ author: "ada", body: "ping", reply: null }))).toBeNull();
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
  ).toBe("`<ada>` has joined");
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

const ident = { model: "Opus 4.8", harness: "Claude Code", host: "studio-mbp-01" };

const metaEv = (over: Partial<SwarmEvent>): SwarmEvent =>
  ev({ event: "meta", type: "meta", author: "bark-vivid", ...over });

test("formatPeerIdent joins model / harness @ host, omitting absent parts", () => {
  expect(formatPeerIdent(ident)).toBe("Opus 4.8 / Claude Code @ studio-mbp-01");
  expect(formatPeerIdent({ model: "Opus 4.8" })).toBe("Opus 4.8");
  expect(formatPeerIdent({ host: "box-1" })).toBe("@ box-1");
  expect(formatPeerIdent({})).toBe("");
});

test("formatMeta: a peer adding /peers/<nick> reads 'runs <ident>'", () => {
  expect(
    formatMeta(
      metaEv({
        patch: [{ op: "add", path: "/peers/bark-vivid", value: ident }],
        document: { peers: { "bark-vivid": ident } },
      }),
    ),
  ).toBe("`<bark-vivid>` runs `Opus 4.8 / Claude Code @ studio-mbp-01`");
});

test("formatMeta: the /peers seed form (keyed value) names each peer", () => {
  expect(
    formatMeta(
      metaEv({
        patch: [{ op: "add", path: "/peers", value: { "bark-vivid": ident } }],
        document: { peers: { "bark-vivid": ident } },
      }),
    ),
  ).toBe("`<bark-vivid>` runs `Opus 4.8 / Claude Code @ studio-mbp-01`");
});

test("formatMeta: a replace reads 'now runs'", () => {
  const next = { ...ident, model: "Opus 4.7" };
  expect(
    formatMeta(
      metaEv({
        patch: [{ op: "replace", path: "/peers/bark-vivid", value: next }],
        document: { peers: { "bark-vivid": next } },
      }),
    ),
  ).toBe("`<bark-vivid>` now runs `Opus 4.7 / Claude Code @ studio-mbp-01`");
});

test("formatMeta: your own report reads 'you reported <ident>' (shown, not dropped)", () => {
  const event = metaEv({
    self: true,
    patch: [{ op: "add", path: "/peers/bark-vivid", value: ident }],
    document: { peers: { "bark-vivid": ident } },
  });
  expect(formatMeta(event)).toBe("you reported `Opus 4.8 / Claude Code @ studio-mbp-01`");
  // formatDisplay must surface meta self-reports (the generic self-drop is bypassed).
  expect(formatDisplay(event)).toBe("you reported `Opus 4.8 / Claude Code @ studio-mbp-01`");
});

test("formatMeta: removing a peer entry reads 'cleared its identity'", () => {
  expect(
    formatMeta(
      metaEv({
        patch: [{ op: "remove", path: "/peers/bark-vivid" }],
        document: { peers: {} },
      }),
    ),
  ).toBe("`<bark-vivid>` cleared its identity");
});

test("formatMeta: a non-/peers meta change falls back to the daemon display", () => {
  expect(
    formatMeta(
      metaEv({
        author: "otter-embark",
        patch: [{ op: "add", path: "/caps/review", value: true }],
        document: { caps: { review: true } },
        display: "🐝️ `<otter-embark>` changed /caps/review",
      }),
    ),
  ).toBe("🐝️ `<otter-embark>` changed /caps/review");
});
