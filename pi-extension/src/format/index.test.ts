import { expect, test } from "bun:test";
import type { SwarmEvent } from "../types";
import {
  engagementKind,
  formatDisplay,
  formatMessage,
  formatMeta,
  formatOutbound,
  formatPeerIdent,
  formatRoster,
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

test("formatMeta: a full identity report reads 'runs <ident>'", () => {
  expect(
    formatMeta(
      metaEv({
        merge: { peers: { "bark-vivid": ident } },
        document: { peers: { "bark-vivid": ident } },
      }),
    ),
  ).toBe("`<bark-vivid>` runs `Opus 4.8 / Claude Code @ studio-mbp-01`");
});

test("formatMeta: a multi-peer merge names each touched peer", () => {
  const other = { model: "GLM 5.2", harness: "pi", host: "box-2" };
  expect(
    formatMeta(
      metaEv({
        merge: { peers: { "bark-vivid": ident, "otter-embark": other } },
        document: { peers: { "bark-vivid": ident, "otter-embark": other } },
      }),
    ),
  ).toBe(
    "`<bark-vivid>` runs `Opus 4.8 / Claude Code @ studio-mbp-01`\n" +
      "`<otter-embark>` runs `GLM 5.2 / pi @ box-2`",
  );
});

test("formatMeta: a partial update (model switch) still reads 'runs' the full identity", () => {
  const next = { ...ident, model: "Opus 4.7" };
  expect(
    formatMeta(
      metaEv({
        // A model switch sends only the changed field — a partial merge — but the
        // rendered identity comes from the post-merge document, so it's complete.
        merge: { peers: { "bark-vivid": { model: "Opus 4.7" } } },
        document: { peers: { "bark-vivid": next } },
      }),
    ),
  ).toBe("`<bark-vivid>` runs `Opus 4.7 / Claude Code @ studio-mbp-01`");
});

test("formatMeta: your own report reads 'you reported <ident>' (shown, not dropped)", () => {
  const event = metaEv({
    self: true,
    merge: { peers: { "bark-vivid": ident } },
    document: { peers: { "bark-vivid": ident } },
  });
  expect(formatMeta(event)).toBe("you reported `Opus 4.8 / Claude Code @ studio-mbp-01`");
  // formatDisplay must surface meta self-reports (the generic self-drop is bypassed).
  expect(formatDisplay(event)).toBe("you reported `Opus 4.8 / Claude Code @ studio-mbp-01`");
});

test("formatMeta: a null merge value reads 'cleared its identity'", () => {
  expect(
    formatMeta(
      metaEv({
        merge: { peers: { "bark-vivid": null } },
        document: { peers: {} },
      }),
    ),
  ).toBe("`<bark-vivid>` cleared its identity");
});

test("formatMeta: a pure status flip reads 'is now <status>'", () => {
  expect(
    formatMeta(
      metaEv({
        merge: { peers: { "bark-vivid": { status: "busy" } } },
        document: { peers: { "bark-vivid": { ...ident, status: "busy" } } },
      }),
    ),
  ).toBe("`<bark-vivid>` is now busy");
});

test("formatMeta: your own status flip reads 'you are now <status>'", () => {
  expect(
    formatMeta(
      metaEv({
        self: true,
        merge: { peers: { "bark-vivid": { status: "idle" } } },
        document: { peers: { "bark-vivid": { ...ident, status: "idle" } } },
      }),
    ),
  ).toBe("you are now idle");
});

test("formatMeta: status seeded alongside identity still reads 'runs <ident>'", () => {
  // The join seed carries model/harness/host *and* status:idle — that's an
  // identity report, not a status flip, so it renders as 'runs', not 'is now'.
  expect(
    formatMeta(
      metaEv({
        merge: { peers: { "bark-vivid": { ...ident, status: "idle" } } },
        document: { peers: { "bark-vivid": { ...ident, status: "idle" } } },
      }),
    ),
  ).toBe("`<bark-vivid>` runs `Opus 4.8 / Claude Code @ studio-mbp-01`");
});

test("formatMeta: a non-/peers meta change falls back to the daemon display", () => {
  expect(
    formatMeta(
      metaEv({
        author: "otter-embark",
        merge: { caps: { review: true } },
        document: { caps: { review: true } },
        display: "💬️ `<otter-embark>` changed /caps/review",
      }),
    ),
  ).toBe("💬️ `<otter-embark>` changed /caps/review");
});

test("formatRoster includes a status column, empty when a peer has not reported", () => {
  const out = formatRoster({
    name: "dealer-lilac",
    count: 3,
    participants: [
      {
        nickname: "swift-cedar",
        reach: "direct",
        model: "Opus 4.8",
        harness: "Claude Code",
        host: "studio-mbp-01",
        status: "idle",
        lastSeenSecsAgo: 3,
        quiet: false,
      },
      {
        nickname: "calm-otter",
        reach: "gossip",
        model: "Opus 4.8",
        harness: "Claude Code",
        host: "dev-box-2",
        status: "busy",
        lastSeenSecsAgo: 12,
        quiet: false,
      },
      {
        nickname: "ghost-elm",
        reach: "gossip",
        lastSeenSecsAgo: 90,
        quiet: true,
      },
    ],
  });
  const [headings, , cedar, otter, elm] = out.split("\n").slice(2);
  expect(headings).toContain("status");
  expect(cedar).toContain("idle");
  expect(otter).toContain("busy");
  // A peer that never reported has an empty status cell (no "idle"/"busy").
  expect(elm).not.toContain("idle");
  expect(elm).not.toContain("busy");
});
