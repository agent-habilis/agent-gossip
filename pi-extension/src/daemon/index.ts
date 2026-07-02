import type { ChildProcess } from "node:child_process";
import * as readline from "node:readline";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { sendTaskLeg } from "../core";
import { engagementKind, formatDisplay } from "../format";
import { runSwarmCommand } from "../helpers";
import { state } from "../state";
import { trackStart, trackStatus } from "../todo";
import type { SwarmEvent } from "../types";
import {
  BEE,
  inject,
  injectSilent,
  notifyError,
  notifyWarning,
  notify as pushDisplay,
} from "../ui";

const BATCH_DELAY_MS = 100;

// Buffer caps: a long session whose UI context is unavailable (reload
// window, stalled extension host) must not accumulate every swarm event
// forever. Oldest events are shed first — the recent ones are the
// relevant ones for a UI — and the shed count is surfaced as one
// "(+N dropped)" notice on the next successful flush.
const PENDING_CAP = 256;
const BATCH_CAP = 128;

// Injected silently once per session (into context, never printed) so the agent
// knows the standing reply policy: answer anything addressed to it, weigh in on
// a broadcast only when it can help.
const REPLY_POLICY =
  "Swarm messages. Reply with swarm_send to any addressed to you (→ you); for a broadcast, reply only when you are ≥90% confident you can help — otherwise take no action. Ping/pong is automatic; never reply to a ping. Replies are plain text, not threaded — do not add the 🐝️, the swarm UI adds it.";

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

  // Deliver the standing policy once per session, into context only — never
  // printed (it used to ride on every batch and cluttered the transcript).
  if (!state.policySent && injectSilent(REPLY_POLICY)) state.policySent = true;

  const myNick = state.session?.nickname;
  const lines = batch.map((message) => {
    const kind = engagementKind(message, myNick);
    if (kind === "state") {
      // Carry the freshly-derived document into the turn so the agent reacts in
      // one go (no separate read). Guidance is task-agnostic — the agent decides.
      const document = JSON.stringify(message.document ?? {}, null, 2);
      return `${BEE} \`<${message.author}>\` changed shared state. New document:
\`\`\`json
${document}
\`\`\`
React per your current task — act only on your turn (check a turn marker in the document), then apply your change with swarm_apply_merge.`;
    }
    return kind === "directed"
      ? `${BEE} \`<${message.author}>\` → you: ${message.body}`
      : `${BEE} \`<${message.author}>\`: ${message.body}`;
  });
  if (!inject(lines.join("\n"))) {
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

  // Tasks (handover or delegated work) are an interactive flow, not a verbatim line.
  if (event.event === "task_progress") return; // keepalive beat — no UI
  if (event.event === "task") {
    handleTaskEvent(event);
    return;
  }

  const output = formatDisplay(event);

  // Directed and broadcast peer messages wake the agent (inject → turn); a
  // reply aimed at another peer, presence, and lifecycle lines are display-only.
  if (engagementKind(event, state.session?.nickname)) {
    if (state.pi) {
      if (state.messageBatch.length >= BATCH_CAP) {
        state.messageBatch.shift();
        state.droppedPending += 1;
      }
      state.messageBatch.push(event);
      if (state.messageBatchTimer) clearTimeout(state.messageBatchTimer);
      state.messageBatchTimer = setTimeout(flushMessageBatch, BATCH_DELAY_MS);
    } else {
      pushPending(event);
    }
  } else if (output) {
    if (state.pi) {
      pushDisplay(output);
    } else {
      pushPending(event);
    }
  }
}

function oneLineSummary(body: string | undefined): string {
  return (
    body
      ?.split("\n")
      .find((line) => line.trim())
      ?.slice(0, 120) ?? "(no brief)"
  );
}

// An `offer` addressed to us with no task we already track: we are the
// receiver. Gate entry through the user (Accept/Decline), then hand the brief
// to the agent.
async function handleIncomingOffer(event: SwarmEvent, taskId: string): Promise<void> {
  const author = event.author ?? "?";
  const summary = oneLineSummary(event.body);
  state.tasks.set(taskId, {
    taskId,
    peer: author,
    role: "receiver",
    summary,
  });

  const ctx = state.ctx;
  if (!ctx) {
    // No UI to decide — decline rather than silently hold the offer open.
    try {
      sendTaskLeg({
        to: author,
        taskId,
        phase: "decline",
        text: "recipient unavailable",
      });
    } catch {
      /* best effort */
    }
    state.tasks.delete(taskId);
    return;
  }

  const choice = await ctx.ui.select(`Incoming task from <${author}>: ${summary}. Take it?`, [
    "Accept",
    "Decline",
  ]);
  if (choice !== "Accept") {
    try {
      sendTaskLeg({ to: author, taskId, phase: "decline" });
    } catch {
      /* best effort */
    }
    state.tasks.delete(taskId);
    trackStatus({ peer: author, role: "receiver", status: "declined" });
    return;
  }

  try {
    sendTaskLeg({ to: author, taskId, phase: "accept" });
  } catch (error) {
    notifyError(`accept failed: ${error instanceof Error ? error.message : "unknown"}`);
    state.tasks.delete(taskId);
    return;
  }

  trackStart({ peer: author, role: "receiver", summary, status: "accepted" });
  const tool = `the swarm_task_leg tool (task_id "${taskId}", to "${author}")`;
  // No wire discriminator: the brief says what is being asked. Cover both
  // usage patterns and let the agent act on what it read.
  inject(
    `You accepted a task from \`<${author}>\`. Brief:\n\n${event.body ?? ""}\n\n` +
      `Ask anything unclear with ${tool} phase "context". ` +
      `If the brief asks you to run work and return a result, do it (confirm with the user first if it makes changes), then send ${tool} phase "done" with your result in the text. ` +
      `If it hands ownership to you (no result expected), send phase "done" when you have what you need; \`<${author}>\` confirms, then the work is yours.`,
  );
}

// Route a `task` event for a leg addressed to us, given what we already
// track for its task_id.
function handleTaskEvent(event: SwarmEvent): void {
  const taskId = event.task_id;
  if (!taskId) return;
  // Our own echo is informational only — records are created at send time.
  if (event.self) return;
  if (!state.session?.nickname || event.to !== state.session.nickname) return;

  const author = event.author ?? "?";
  const existing = state.tasks.get(taskId);

  if (event.phase === "offer") {
    if (!existing) void handleIncomingOffer(event, taskId);
    return;
  }
  if (!existing) return; // a leg for a task we don't track

  // Only initiators carry a local flow label (from the tool they invoked); a
  // received offer has none, so receiver-side handling stays flow-agnostic.
  const kind = existing.kind;
  switch (event.phase) {
    case "context":
      inject(
        `Question from \`<${author}>\` on task ${taskId}: ${event.body ?? ""}\n` +
          `Answer with the swarm_task_leg tool phase "context" (task_id "${taskId}", to "${author}").`,
      );
      break;
    case "accept":
      trackStatus({ kind, peer: author, role: existing.role, status: "accepted" });
      break;
    case "decline":
      trackStatus({
        kind,
        peer: author,
        role: existing.role,
        status: event.body ? `declined: ${event.body}` : "declined",
      });
      state.tasks.delete(taskId);
      break;
    case "done":
      // Initiator side of a handover: nothing to verify, so auto-confirm.
      if (existing.role === "initiator" && kind === "handover") {
        try {
          sendTaskLeg({ to: author, taskId, phase: "confirm" });
        } catch {
          /* best effort */
        }
        trackStatus({ kind, peer: author, role: existing.role, status: "done" });
        state.tasks.delete(taskId);
      } else if (existing.role === "initiator") {
        // Task result returns here (Phase F drives the confirm/change loop).
        inject(
          `\`<${author}>\` returned the task result (task ${taskId}):\n\n${event.body ?? ""}\n\n` +
            `If it meets the criteria, send the swarm_task_leg tool phase "confirm" (task_id "${taskId}", to "${author}"); otherwise phase "change" with feedback.`,
        );
      }
      break;
    case "change":
      // Receiver: the initiator wants a revision of the returned result.
      if (existing.role === "receiver") {
        inject(
          `\`<${author}>\` asked for a revision on task ${taskId}: ${event.body ?? ""}\n` +
            `Revise and re-send the swarm_task_leg tool phase "done" (task_id "${taskId}", to "${author}") with the updated result.`,
        );
      }
      break;
    case "confirm":
      // Receiver side: the close is confirmed. If the brief handed ownership
      // (no result was returned), the work is now the receiver's to do; if a
      // result was returned, it was accepted and there is nothing more to do.
      if (existing.role === "receiver") {
        inject(
          `Task ${taskId} confirmed by \`<${author}>\`. If you returned a result, it was accepted — nothing more to do. If this handed ownership to you, do the work now (confirm with the user first if it makes changes); it is yours and is not reported back.`,
        );
        trackStatus({ kind, peer: author, role: existing.role, status: "confirmed" });
      }
      state.tasks.delete(taskId);
      break;
    case "cancel":
      trackStatus({ kind, peer: author, role: existing.role, status: "cancelled" });
      state.tasks.delete(taskId);
      break;
    default:
      break;
  }
}

// Attach the single readline that both detects the `ready` line and forwards
// every other line to `processDaemonLine`. One reader (not a `waitForReady`
// reader followed by a second one): a second readline would lose the `joined`
// events the daemon bundles into the same stdout chunk as `ready` — the peers
// already present when we join. Resolves with the raw `ready` line.
export function startWatcher({
  child,
  timeoutMs,
}: {
  child: ChildProcess;
  timeoutMs: number;
}): Promise<string> {
  state.daemon = child;
  const stdout = child.stdout;
  if (!stdout) {
    return Promise.reject(new Error("daemon spawned without a stdout stream"));
  }
  return new Promise<string>((resolve, reject) => {
    const lineReader = readline.createInterface({ input: stdout });
    let settled = false;
    const timeout = setTimeout(() => {
      if (settled) return;
      settled = true;
      lineReader.close();
      child.kill();
      reject(new Error(`timeout waiting for ready event (${timeoutMs}ms)`));
    }, timeoutMs);

    lineReader.on("line", (line) => {
      if (!settled && line.includes('"event":"ready"')) {
        settled = true;
        clearTimeout(timeout);
        resolve(line);
        return;
      }
      processDaemonLine(line);
    });
    child.on("error", (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timeout);
      lineReader.close();
      reject(new Error(`spawn failed: ${error.message}`));
    });
    child.on("exit", () => {
      state.daemon = null;
    });
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

export function flushPending(_ctx: ExtensionContext): void {
  if (state.droppedPending > 0) {
    notifyWarning(
      `${state.droppedPending} earlier swarm event(s) were dropped while the UI was unavailable`,
    );
    state.droppedPending = 0;
  }
  const messages = state.pendingMessages.splice(0);
  for (const message of messages) {
    const output = formatDisplay(message);
    if (output) pushDisplay(output);
  }
}
