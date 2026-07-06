import { type ExtensionAPI, type Theme, getMarkdownTheme } from "@earendil-works/pi-coding-agent";
import { Markdown, Text } from "@earendil-works/pi-tui";
import { state } from "../state";

// Every mesh line is prefixed with this, applied once in `send()` — call
// sites never write it themselves. Exported for `flushMessageBatch`, which
// builds one bee per peer line.
export const BEE = "💬️";

// The single output library for the extension: everything user- or
// agent-facing goes through `pi.sendMessage` here, never `sendUserMessage`.
// Two intents share one primitive:
//   - print (notify/notifyBlock/notifyWarning/notifyError): a 💬 line shown
//     inline in the transcript. When idle it renders immediately with no LLM
//     turn — a peer joining or a status table shouldn't make the model respond;
//     while streaming it rides in on `nextTurn`, shown without interrupting.
//   - inject: a brief the agent must act on (handover/task offers and their
//     leg prompts, auto-replies). It always triggers a turn — `triggerTurn` when
//     idle, a `steer` to break into a running stream. The harness converts the custom
//     message into a `user` message for the model, so this drives the agent
//     exactly as a typed message would.
//
// `notify`/`inject` render as markdown so backticked identifiers (`<nick>`,
// `#mesh`) read as inline code; `notifyBlock` renders plain for preformatted,
// column-aligned output (rosters, status dumps) that markdown reflow would break.
export function notify(text: string): void {
  send("mesh-info", text, false);
}

export function notifyBlock(text: string): void {
  send("mesh-block", text, false);
}

export function notifyWarning(text: string): void {
  send("mesh-warning", text, false);
}

export function notifyError(text: string): void {
  send("mesh-error", text, false);
}

// Returns false when the message couldn't be delivered (no session, or the
// send threw) so a caller can re-queue — see `flushMessageBatch`.
export function inject(text: string): boolean {
  return send("mesh-inject", text, true);
}

// Add to the model's context without rendering it or forcing a turn — for
// standing instructions the agent should know but the user shouldn't see.
// `display: false` keeps it out of the transcript (the harness still feeds it to
// the model as a user message); no trigger means it's context, not an action.
// No bee prefix and no renderer: it is never shown.
export function injectSilent(text: string): boolean {
  const pi = state.pi;
  if (!pi) return false;
  const idle = state.ctx?.isIdle?.() ?? true;
  try {
    pi.sendMessage(
      { customType: "mesh-context", content: text, display: false },
      idle ? {} : { deliverAs: "nextTurn" },
    );
    return true;
  } catch {
    return false;
  }
}

function send(customType: string, text: string, trigger: boolean): boolean {
  const pi = state.pi;
  if (!pi) return false;
  const idle = state.ctx?.isIdle?.() ?? true;
  // The batch path bakes its own per-line bees, so guard against doubling.
  const content = text.startsWith(BEE) ? text : `${BEE} ${text}`;
  const message = { customType, content, display: true };
  try {
    if (trigger) {
      pi.sendMessage(message, idle ? { triggerTurn: true } : { deliverAs: "steer" });
    } else {
      pi.sendMessage(message, idle ? {} : { deliverAs: "nextTurn" });
    }
    return true;
  } catch {
    return false;
  }
}

// 💬 lines read like ordinary transcript lines (no `Box`/background — the
// default message framing handles padding). info/inject go through pi's own
// Markdown renderer so inline code spans (and fenced blocks) render like every
// other message; block stays plain to preserve column alignment; warning yellow,
// error red.
export function registerMessageRenderers(pi: ExtensionAPI): void {
  const renderMarkdown = (content: string, theme: Theme) =>
    new Markdown(content, 0, 0, getMarkdownTheme(), {
      color: (text) => theme.fg("customMessageText", text),
    });

  pi.registerMessageRenderer("mesh-info", (message, _options, theme) => {
    const content = typeof message.content === "string" ? message.content : "";
    return renderMarkdown(content, theme);
  });
  pi.registerMessageRenderer("mesh-inject", (message, _options, theme) => {
    const content = typeof message.content === "string" ? message.content : "";
    return renderMarkdown(content, theme);
  });
  pi.registerMessageRenderer("mesh-block", (message) => {
    const content = typeof message.content === "string" ? message.content : "";
    return new Text(content, 0, 0);
  });
  pi.registerMessageRenderer("mesh-warning", (message, _options, theme) => {
    const content = typeof message.content === "string" ? message.content : "";
    return new Text(theme.fg("warning", content), 0, 0);
  });
  pi.registerMessageRenderer("mesh-error", (message, _options, theme) => {
    const content = typeof message.content === "string" ? message.content : "";
    return new Text(theme.fg("error", content), 0, 0);
  });
}
