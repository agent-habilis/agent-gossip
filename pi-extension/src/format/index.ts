import type { Peer, PingResult, SwarmEvent } from "../types";

export function formatPresence(event: SwarmEvent): string | null {
  if (event.subtype === "alive") return null;
  if (event.subtype === "joined") {
    // Presence carries no metadata: the daemon never ships model/harness/host on
    // the join event — they live in the `meta` channel and only after the joiner
    // self-reports (which happens just *after* it joins). The roster
    // (`/swarm-status`, the handover/task pickers) is where that surfaces.
    return `\`<${event.author}>\` has joined`;
  }
  if (event.subtype === "left") return `\`<${event.author}>\` has left`;
  return null;
}

export function formatMessage(event: SwarmEvent): string | null {
  if (!event.body) return null;

  // Ambient ping/pong is handled silently (the daemon/extension auto-pongs and
  // RTT is tracked for /swarm-ping) — never surfaced, matching the Claude Code
  // plugin which leaves ping/pong entirely to the daemon.
  if (event.body === "ping" && !event.reply) return null;
  if (event.body === "pong") return null;

  if (event.reply) {
    return `\`<${event.author}>\` → \`<${event.reply}>\`: ${event.body}`;
  }

  return `\`<${event.author}>\`: ${event.body}`;
}

// Our own sent message, echoed into the transcript (the daemon filters self
// from its stream, so we surface it here). No bee — `notify` prepends it.
export function formatOutbound(nick: string, text: string, reply?: string): string {
  return reply ? `\`<${nick}>\` → \`<${reply}>\`: ${text}` : `\`<${nick}>\`: ${text}`;
}

// A peer's shared-state change, terse like the other formatters (the document
// itself rides on the wake text in `flushMessageBatch`, not here).
export function formatState(event: SwarmEvent): string {
  return `\`<${event.author}>\` changed shared state`;
}

export function formatPeerLifecycle(event: SwarmEvent): string | null {
  if (event.event === "peer_timeout") {
    return `\`<${event.nickname}>\` went quiet`;
  }
  if (event.event === "peer_return") {
    return `\`<${event.nickname}>\` came back`;
  }
  return null;
}

// What a peer runs on, as one line: `model / harness @ host`, omitting absent
// parts. Shared by the `/swarm` roster picker and `formatMeta` so every surface
// renders identity identically.
export function formatPeerIdent(peer: { model?: string; harness?: string; host?: string }): string {
  const stack = [peer.model, peer.harness].filter(Boolean).join(" / ");
  return peer.host ? (stack ? `${stack} @ ${peer.host}` : `@ ${peer.host}`) : stack;
}

// A `meta` event: surface a peer self-reporting/updating what it runs on, the
// way a join line surfaces arrival. Model/harness/host land in the `meta`
// channel under `/peers/<nick>` *after* a peer joins, so this is where they
// become visible. Binary-opaque: the daemon never formats these values; the
// `/peers` convention lives here. The applied RFC 7386 merge is `event.merge`,
// so each key under `merge.peers` is a touched nick; the post-merge identity is
// read from `event.document.peers[nick]`. A `null` merge value (or a nick gone
// from the document) is a clear; otherwise the peer "runs" the identity. (We
// don't distinguish a first report from an update: the merge carries no
// before-state, and a partial merge — e.g. a model-only switch — is
// indistinguishable from an incomplete first report, so a single verb avoids
// mislabeling either.)
export function formatMeta(event: SwarmEvent): string | null {
  const peersDoc = (event.document?.peers ?? {}) as Record<
    string,
    { model?: string; harness?: string; host?: string; status?: string }
  >;
  const mergePeers = (event.merge?.peers ?? {}) as Record<string, unknown>;
  const touched = Object.keys(mergePeers);
  // A meta change that doesn't touch `/peers` (some other convention) — fall
  // back to the daemon's value-blind path summary so it still surfaces.
  if (touched.length === 0) {
    return event.display ?? `\`<${event.author}>\` changed swarm metadata`;
  }
  const isSelf = Boolean(event.self);
  const lines = touched.map((nick) => {
    const mergeVal = mergePeers[nick];
    const peer = peersDoc[nick];
    if (mergeVal === null || !peer) {
      return isSelf ? "you cleared your identity" : `\`<${nick}>\` cleared its identity`;
    }
    // A pure status flip (the merge touched only `status`) surfaces availability,
    // not identity — e.g. `<nick>` is now busy. A `status` seeded alongside
    // identity fields falls through to the identity line below.
    if (
      typeof mergeVal === "object" &&
      Object.keys(mergeVal as object).length === 1 &&
      "status" in (mergeVal as object)
    ) {
      const status = peer.status ?? String((mergeVal as { status?: unknown }).status ?? "");
      return isSelf ? `you are now ${status}` : `\`<${nick}>\` is now ${status}`;
    }
    // Render the identity as an inline code span (backticks), like the `<nick>`.
    const ident = formatPeerIdent(peer);
    if (isSelf) return ident ? `you reported \`${ident}\`` : "you reported your identity";
    return ident ? `\`<${nick}>\` runs \`${ident}\`` : `\`<${nick}>\` reported its identity`;
  });
  return [...new Set(lines)].join("\n");
}

export function formatDisplay(event: SwarmEvent): string | null {
  if (event.event === "info" || event.event === "error") return null;
  // Meta is handled before the self-drop below: unlike other events, a peer's
  // *own* identity report is worth showing (`you reported …`), mirroring how the
  // state channel shows your own `you changed …`.
  if (event.type === "meta") return formatMeta(event);
  if (event.self) return null;

  if (event.type === "presence") return formatPresence(event);
  if (event.type === "msg") return formatMessage(event);
  if (event.type === "state") return formatState(event);

  return formatPeerLifecycle(event);
}

export function formatRoster({
  name,
  count,
  participants,
}: {
  name: string;
  count: number;
  participants: Peer[];
}): string {
  const header = `#${name} · ${count} participants`;
  if (participants.length === 0) return `${header}\n(just you — no peers yet)`;
  // Rendered via `notifyBlock` (plain text, no markdown), so align columns by
  // padding rather than emitting a markdown table; nicks stay plain here (no
  // backticks) — markdown reflow would break the alignment.
  const headings = ["peer", "connection", "model", "harness", "host", "status", "last seen"];
  const rows = participants.map((peer) => [
    peer.nickname,
    peer.reach === "direct" ? "connected" : "gossip",
    peer.model ?? "",
    peer.harness ?? "",
    peer.host ?? "",
    peer.status ?? "",
    peer.lastSeenSecsAgo == null
      ? "—"
      : `${peer.quiet ? "quiet · " : ""}${peer.lastSeenSecsAgo}s ago`,
  ]);
  const widths = headings.map((heading, column) =>
    Math.max(heading.length, ...rows.map((row) => row[column].length)),
  );
  const line = (cells: string[]) =>
    cells
      .map((cell, column) => cell.padEnd(widths[column]))
      .join("  ")
      .trimEnd();
  const separator = widths.map((width) => "─".repeat(width)).join("  ");
  return [header, "", line(headings), separator, ...rows.map(line)].join("\n");
}

// Shared RTT report for /swarm-ping and the swarm_ping tool — one source so the
// two never drift. No bee prefix (the UI/agent adds it). The footer counts
// responders only; pi has no reliable known-peer total at this point (a peer
// can pong yet be absent from a post-wait roster), so it deliberately omits a
// denominator rather than print a fabricated or impossible one.
export function formatPingReport(results: PingResult[]): string {
  if (results.length === 0) return "ping: no peers responded";
  const rows = results.map((result) => `| \`<${result.author}>\` | ${result.rtt}ms |`);
  return ["ping", "", "| peer | RTT |", "|---|---|", ...rows, "", `${results.length} online`].join(
    "\n",
  );
}

export type EngagementKind = "directed" | "broadcast" | "state";

// Whether an incoming peer event should wake the agent, and how. "directed"
// when a message is addressed to us (reply === our nick) — always answer;
// "broadcast" when it went to the whole swarm — answer only if we can help;
// "state" when a peer changed the shared state — react per the current task.
// null means no engagement: our own echo, ping/pong, a reply aimed at another
// peer, or a non-message (presence/lifecycle) — those are display-only.
export function engagementKind(
  event: SwarmEvent,
  myNick: string | undefined,
): EngagementKind | null {
  // A peer's shared-state change wakes us so we can react to the new document;
  // our own change does not (no self-loop). Checked first — state events carry
  // no `body`, so the message guard below would otherwise drop them.
  if (event.type === "state") return event.self ? null : "state";
  if (event.type !== "msg" || event.self || !event.body) return null;
  if (event.body === "ping" || event.body === "pong") return null;
  if (event.reply) return event.reply === myNick ? "directed" : null;
  return "broadcast";
}
