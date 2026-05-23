import { state } from "./state";
import type { NotifyType, SwarmEvent } from "./types";

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
  if (event.type === "presence") {
    if (event.subtype === "left") return "warning";
    return "info";
  }
  if (event.event === "peer_timeout") return "error";
  return "info";
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
