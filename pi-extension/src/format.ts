import { state } from "./state";
import type { NotifyType, Peer, SwarmEvent } from "./types";

export function formatPresence(event: SwarmEvent): string | null {
  if (event.subtype === "alive") return null;
  if (event.subtype === "joined") return `🐝 <${event.author}> joined`;
  if (event.subtype === "left") return `🐝 <${event.author}> left`;
  return null;
}

export function formatMessage(event: SwarmEvent): string | null {
  if (!event.body) return null;

  if (event.body === "ping" && !event.reply) {
    return "🐝 ping → pong";
  }

  if (event.body === "pong") {
    if (state.pingPending) return null;
    return `🐝 pong from <${event.author}>`;
  }

  if (event.reply) {
    return `🐝 <${event.author}> → <${event.reply}>: ${event.body}`;
  }

  return `🐝 <${event.author}>: ${event.body}`;
}

export function formatPeerLifecycle(event: SwarmEvent): string | null {
  if (event.event === "peer_timeout") {
    return `🐝 <${event.nickname}> went quiet`;
  }
  if (event.event === "peer_return") {
    return `🐝 <${event.nickname}> came back`;
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

export function getNotifyType(event: SwarmEvent): NotifyType {
  // Presence (joined/left) is plain "info" — a peer leaving is not a warning,
  // so pi must not prefix the line with "Warning:".
  if (event.type === "presence") return "info";
  if (event.event === "peer_timeout") return "error";
  return "info";
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
  const header = `🐝 #${name} · ${count} participants`;
  if (participants.length === 0) return `${header}\n(just you — no peers yet)`;
  // pi's notify renders plain text (no markdown), so align columns by padding
  // rather than emitting a markdown table.
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

export function isQuestion(event: SwarmEvent): boolean {
  return (
    event.type === "msg" &&
    !event.reply &&
    !event.self &&
    !!event.body &&
    event.body !== "ping" &&
    event.body !== "pong"
  );
}
