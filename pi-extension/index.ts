import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { registerCommands } from "./src/commands";
import { clearBatch, flushPending } from "./src/daemon";
import { state } from "./src/state";
import { registerTools } from "./src/tools";

export default function register(pi: ExtensionAPI) {
  state.pi = pi;

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
