import type { Peer, PingResult, SwarmEvent } from "../types";

export function formatPresence(event: SwarmEvent): string | null {
  if (event.subtype === "alive") return null;
  if (event.subtype === "joined") {
    // The daemon ships the joiner's model/harness as structured fields; show
    // them in parens right after the nick, matching the Claude Code plugin's
    // join line verbatim.
    const meta = [event.model, event.harness].filter(Boolean).join(" / ");
    return meta ? `\`<${event.author}>\` (${meta}) has joined` : `\`<${event.author}>\` has joined`;
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

export function formatDisplay(event: SwarmEvent): string | null {
  if (event.self) return null;
  if (event.event === "info" || event.event === "error") return null;

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
  const headings = ["peer", "connection", "model", "harness", "last seen"];
  const rows = participants.map((peer) => [
    peer.nickname,
    peer.reach === "direct" ? "connected" : "gossip",
    peer.model ?? "",
    peer.harness ?? "",
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
