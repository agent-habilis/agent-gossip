import type { ChildProcess } from "node:child_process";
import * as readline from "node:readline";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { formatDisplay, getNotifyType, isQuestion } from "./format";
import { runSwarmCommand } from "./helpers";
import { state } from "./state";
import type { SwarmEvent } from "./types";

const BATCH_DELAY_MS = 100;

// Buffer caps: a long session whose UI context is unavailable (reload
// window, stalled extension host) must not accumulate every swarm event
// forever. Oldest events are shed first — the recent ones are the
// relevant ones for a UI — and the shed count is surfaced as one
// "(+N dropped)" notice on the next successful flush.
const PENDING_CAP = 256;
const BATCH_CAP = 128;

function pushPending(event: SwarmEvent): void {
  if (state.pendingMessages.length >= PENDING_CAP) {
    state.pendingMessages.shift();
    state.droppedPending += 1;
  }
  state.pendingMessages.push(event);
}

function handleAutoPingReply(event: SwarmEvent): void {
  if (event.type !== "msg" || event.body !== "ping" || event.reply || !event.author) return;
  const session = state.session;
  if (!session) return;

  try {
    runSwarmCommand([
      "msg",
      "--swarm",
      session.swarm,
      "--nickname",
      session.nickname,
      "--text",
      "pong",
      "--reply",
      event.author,
    ]);
  } catch {
    /* ok */
  }
}

function handlePongTracking(event: SwarmEvent): boolean {
  if (event.type !== "msg" || event.body !== "pong" || !state.pingPending || !event.author)
    return false;

  const roundTripTime = Date.now() - state.pingStartTime;
  if (!state.pongMap.has(event.author)) state.pongMap.set(event.author, roundTripTime);
  return true;
}

export function flushMessageBatch(): void {
  if (state.messageBatchTimer) {
    clearTimeout(state.messageBatchTimer);
    state.messageBatchTimer = null;
  }
  const batch = state.messageBatch.splice(0);
  if (batch.length === 0 || !state.pi) return;

  const lines = batch.map((message) => `🐝 <${message.author}>: ${message.body}`);
  const text = lines.join("\n");

  try {
    if (state.ctx?.isIdle?.() ?? true) {
      state.pi.sendUserMessage(text);
    } else {
      state.pi.sendUserMessage(text, { deliverAs: "steer" });
    }
  } catch {
    for (const message of batch) pushPending(message);
  }
}

export function processDaemonLine(line: string): void {
  let event: SwarmEvent;
  try {
    event = JSON.parse(line);
  } catch {
    return;
  }

  handleAutoPingReply(event);
  if (handlePongTracking(event)) return;

  const output = formatDisplay(event);

  if (isQuestion(event)) {
    if (state.autoReply && state.pi) {
      if (state.messageBatch.length >= BATCH_CAP) {
        state.messageBatch.shift();
        state.droppedPending += 1;
      }
      state.messageBatch.push(event);
      if (state.messageBatchTimer) clearTimeout(state.messageBatchTimer);
      state.messageBatchTimer = setTimeout(flushMessageBatch, BATCH_DELAY_MS);
    } else if (output) {
      pushPending(event);
    }
  } else if (output) {
    if (state.ctx) {
      state.ctx.ui.notify(output, getNotifyType(event));
    } else {
      pushPending(event);
    }
  }
}

export function startWatcher(child: ChildProcess): void {
  state.daemon = child;
  const stdout = child.stdout;
  if (!stdout) throw new Error("daemon spawned without a stdout stream");
  const lineReader = readline.createInterface({ input: stdout });
  lineReader.on("line", processDaemonLine);
  child.on("exit", () => {
    state.daemon = null;
  });
}

export function stopWatcher(): void {
  if (state.daemon) {
    try {
      state.daemon.kill();
    } catch {
      /* ok */
    }
    state.daemon = null;
  }
}

export function clearBatch(): void {
  if (state.messageBatchTimer) {
    clearTimeout(state.messageBatchTimer);
    state.messageBatchTimer = null;
  }
  state.messageBatch = [];
}

export function flushPending(ctx: ExtensionContext): void {
  if (state.droppedPending > 0) {
    ctx.ui.notify(
      `🐝 ${state.droppedPending} earlier swarm event(s) were dropped while the UI was unavailable`,
      "warning",
    );
    state.droppedPending = 0;
  }
  const messages = state.pendingMessages.splice(0);
  for (const message of messages) {
    const output = formatDisplay(message);
    if (output) ctx.ui.notify(output, getNotifyType(message));
  }
}
