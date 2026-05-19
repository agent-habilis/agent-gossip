import type { ChildProcess } from "node:child_process";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { Session, SwarmEvent } from "./types";

export type AppState = {
  daemon: ChildProcess | null;
  session: Session | null;
  autoReply: boolean;
  pendingMessages: SwarmEvent[];
  messageBatch: SwarmEvent[];
  messageBatchTimer: ReturnType<typeof setTimeout> | null;
  pi: ExtensionAPI | null;
  ctx: ExtensionContext | null;
  pingPending: boolean;
  pingStartTime: number;
  pongMap: Map<string, number>;
  stateFileId: string | undefined;
};

export const state: AppState = {
  daemon: null,
  session: null,
  autoReply: true,
  pendingMessages: [],
  messageBatch: [],
  messageBatchTimer: null,
  pi: null,
  ctx: null,
  pingPending: false,
  pingStartTime: 0,
  pongMap: new Map(),
  stateFileId: undefined,
};

export function stateFilePath(): string | null {
  if (!state.stateFileId) return null;
  return `/tmp/agent-habilis-swarm-state-${state.stateFileId}.json`;
}
