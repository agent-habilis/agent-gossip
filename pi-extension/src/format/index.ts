import { state } from "../state";
import type { Peer, SwarmEvent } from "../types";

export function formatPresence(event: SwarmEvent): string | null {
  if (event.subtype === "alive") return null;
  if (event.subtype === "joined") {
    // The daemon ships the joiner's model/harness as structured fields; show
    // them in parens right after the nick (matching the Claude Code plugin's
    // join line), keeping pi's own terse verb.
    const meta = [event.model, event.harness].filter(Boolean).join(" / ");
    return meta ? `\`<${event.author}>\` (${meta}) joined` : `\`<${event.author}>\` joined`;
  }
  if (event.subtype === "left") return `\`<${event.author}>\` left`;
  return null;
}

export function formatMessage(event: SwarmEvent): string | null {
  if (!event.body) return null;

  if (event.body === "ping" && !event.reply) {
    return "ping → pong";
  }

  if (event.body === "pong") {
    if (state.pingPending) return null;
    return `pong from \`<${event.author}>\``;
  }

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

export type EngagementKind = "directed" | "broadcast";

// Whether an incoming peer message should wake the agent, and how. "directed"
// when it is addressed to us (reply === our nick) — always answer; "broadcast"
// when it went to the whole swarm — answer only if we can help. null means no
// engagement: our own echo, ping/pong, a reply aimed at another peer, or a
// non-message (presence/lifecycle) — those are display-only.
export function engagementKind(
  event: SwarmEvent,
  myNick: string | undefined,
): EngagementKind | null {
  if (event.type !== "msg" || event.self || !event.body) return null;
  if (event.body === "ping" || event.body === "pong") return null;
  if (event.reply) return event.reply === myNick ? "directed" : null;
  return "broadcast";
}
