import type { ChildProcess } from "node:child_process";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import type { MeshEvent, Session, TaskRecord } from "../types";

export type AppState = {
  daemon: ChildProcess | null;
  session: Session | null;
  pendingMessages: MeshEvent[];
  // Events shed from the capped buffers while the UI was unavailable;
  // surfaced as one "(+N dropped)" notice on the next flush.
  droppedPending: number;
  messageBatch: MeshEvent[];
  messageBatchTimer: ReturnType<typeof setTimeout> | null;
  pi: ExtensionAPI | null;
  ctx: ExtensionContext | null;
  pingPending: boolean;
  pingStartTime: number;
  pongMap: Map<string, number>;
  // In-flight tasks this node is a party to, keyed by task_id.
  tasks: Map<string, TaskRecord>;
  stateFileId: string | undefined;
  // The standing reply policy is injected (silently) once per mesh session.
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
  tasks: new Map(),
  stateFileId: undefined,
  policySent: false,
};

export function stateFilePath(): string | null {
  if (!state.stateFileId) return null;
  // Under the daemon's per-user runtime base `/tmp/agent-mesh-<uid>/` (0700,
  // owned by us) — the same tree `agent-mesh session`/`leave` scan, and
  // matching the daemon's `runtime_base()`. Use the *effective* uid to match
  // the Rust `current_uid()` (`geteuid`) and the skills' `id -u`: under a setuid
  // context `getuid` (real) would diverge, putting this path outside the base
  // the daemon validates and the CLI scans. `geteuid` is unix-only, which this
  // extension already is.
  const uid = process.geteuid?.() ?? process.getuid?.() ?? 0;
  return `/tmp/agent-mesh-${uid}/sessions/${state.stateFileId}.json`;
}
