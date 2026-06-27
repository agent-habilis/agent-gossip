import { type ExtensionAPI, type Theme, getMarkdownTheme } from "@earendil-works/pi-coding-agent";
import { Markdown, Text } from "@earendil-works/pi-tui";
import { state } from "./state";

// Every swarm line is prefixed with this, applied once in `send()` — call
// sites never write it themselves. Exported for `flushMessageBatch`, which
// builds one bee per peer line.
export const BEE = "🐝";

// The single output library for the extension: everything user- or
// agent-facing goes through `pi.sendMessage` here, never `sendUserMessage`.
// Two intents share one primitive:
//   - print (notify/notifyBlock/notifyWarning/notifyError): a 🐝 line shown
//     inline in the transcript. When idle it renders immediately with no LLM
//     turn — a peer joining or a status table shouldn't make the model respond;
//     while streaming it rides in on `nextTurn`, shown without interrupting.
//   - inject: a brief the agent must act on (handover/task, exchange prompts,
//     auto-replies). It always triggers a turn — `triggerTurn` when idle, a
//     `steer` to break into a running stream. The harness converts the custom
//     message into a `user` message for the model, so this drives the agent
//     exactly as a typed message would.
//
// `notify`/`inject` render as markdown so backticked identifiers (`<nick>`,
// `#swarm`) read as inline code; `notifyBlock` renders plain for preformatted,
// column-aligned output (rosters, status dumps) that markdown reflow would break.
export function notify(text: string): void {
  send("swarm-info", text, false);
}

export function notifyBlock(text: string): void {
  send("swarm-block", text, false);
}

export function notifyWarning(text: string): void {
  send("swarm-warning", text, false);
}

export function notifyError(text: string): void {
  send("swarm-error", text, false);
}

// Returns false when the message couldn't be delivered (no session, or the
// send threw) so a caller can re-queue — see `flushMessageBatch`.
export function inject(text: string): boolean {
  return send("swarm-inject", text, true);
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

// 🐝 lines read like ordinary transcript lines (no `Box`/background — the
// default message framing handles padding). info/inject go through pi's own
// Markdown renderer so inline code spans (and fenced blocks) render like every
// other message; block stays plain to preserve column alignment; warning yellow,
// error red.
export function registerMessageRenderers(pi: ExtensionAPI): void {
  const renderMarkdown = (content: string, theme: Theme) =>
    new Markdown(content, 0, 0, getMarkdownTheme(), {
      color: (text) => theme.fg("customMessageText", text),
    });

  pi.registerMessageRenderer("swarm-info", (message, _options, theme) => {
    const content = typeof message.content === "string" ? message.content : "";
    return renderMarkdown(content, theme);
  });
  pi.registerMessageRenderer("swarm-inject", (message, _options, theme) => {
    const content = typeof message.content === "string" ? message.content : "";
    return renderMarkdown(content, theme);
  });
  pi.registerMessageRenderer("swarm-block", (message) => {
    const content = typeof message.content === "string" ? message.content : "";
    return new Text(content, 0, 0);
  });
  pi.registerMessageRenderer("swarm-warning", (message, _options, theme) => {
    const content = typeof message.content === "string" ? message.content : "";
    return new Text(theme.fg("warning", content), 0, 0);
  });
  pi.registerMessageRenderer("swarm-error", (message, _options, theme) => {
    const content = typeof message.content === "string" ? message.content : "";
    return new Text(theme.fg("error", content), 0, 0);
  });
}
