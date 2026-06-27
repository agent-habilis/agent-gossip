import type { ChildProcess } from "node:child_process";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { ExchangeRecord, Session, SwarmEvent } from "../types";

export type AppState = {
  daemon: ChildProcess | null;
  session: Session | null;
  pendingMessages: SwarmEvent[];
  // Events shed from the capped buffers while the UI was unavailable;
  // surfaced as one "(+N dropped)" notice on the next flush.
  droppedPending: number;
  messageBatch: SwarmEvent[];
  messageBatchTimer: ReturnType<typeof setTimeout> | null;
  pi: ExtensionAPI | null;
  ctx: ExtensionContext | null;
  pingPending: boolean;
  pingStartTime: number;
  pongMap: Map<string, number>;
  // In-flight exchanges this node is a party to, keyed by exchange_id.
  exchanges: Map<string, ExchangeRecord>;
  stateFileId: string | undefined;
  // The standing reply policy is injected (silently) once per swarm session.
  policySent: boolean;
};

export const state: AppState = {
  daemon: null,
  session: null,
  pendingMessages: [],
  droppedPending: 0,
  messageBatch: [],
  messageBatchTimer: null,
  pi: null,
  ctx: null,
  pingPending: false,
  pingStartTime: 0,
  pongMap: new Map(),
  exchanges: new Map(),
  stateFileId: undefined,
  policySent: false,
};

export function stateFilePath(): string | null {
  if (!state.stateFileId) return null;
  // Under the shared /tmp/agent-habilis/swarm/ namespace, next to the
  // sockets/logs and the Claude Code plugin's PPID-keyed session files,
  // so a statusline can scan one sessions dir for every member.
  return `/tmp/agent-habilis/swarm/sessions/${state.stateFileId}.json`;
}
