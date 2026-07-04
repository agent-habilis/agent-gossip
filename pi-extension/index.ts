import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { registerCommands } from "./src/commands";
import { clearBatch, flushPending } from "./src/daemon";
import { state } from "./src/state";
import { registerTools } from "./src/tools";
import { registerMessageRenderers } from "./src/ui";

export default function register(pi: ExtensionAPI) {
  state.pi = pi;

  // Inline transcript renderers for the 💬 display lines (see src/ui.ts):
  // info plain, warning yellow, error red — same layout as a peer message.
  registerMessageRenderers(pi);

  pi.on("session_start", (_event, ctx) => {
    state.ctx = ctx;
    state.stateFileId = ctx.sessionManager.getSessionId();
  });

  pi.on("turn_start", (_event, ctx) => {
    state.ctx = ctx;
    if (state.session?.swarm && state.pendingMessages.length > 0) {
      flushPending(ctx);
    }
  });

  pi.on("session_shutdown", () => {
    state.ctx = null;
    clearBatch();
  });

  registerCommands(pi);
  registerTools(pi);
}
