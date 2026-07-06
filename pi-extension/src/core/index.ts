import { execFileSync, spawn } from "node:child_process";
import { hostname } from "node:os";
import * as readline from "node:readline";
import { clearBatch, startWatcher, stopWatcher } from "../daemon";
import { isValidBody, isValidMeshName, runMeshCommand } from "../helpers";
import { state, stateFilePath } from "../state";
import type { DiscoveredMesh, Peer, PingResult, Session } from "../types";

export function cleanup(): void {
  stopWatcher();
  clearBatch();
  state.session = null;
  state.pendingMessages = [];
  state.droppedPending = 0;
  state.pingPending = false;
  state.pongMap.clear();
  state.policySent = false;
}

export type CreateOptions = {
  // Omit/empty ⇒ the daemon mints a random `word-word` name.
  name?: string;
  network?: "private" | "public";
  // undefined ⇒ relay off; "" ⇒ default n0 ladder; "a,b" ⇒ custom ladder.
  relay?: string;
  mdns?: boolean;
  dht?: boolean;
  // List the mesh in a directory; requires network "public".
  advertise?: boolean;
  // Directory to advertise into; omit for the well-known `global`.
  directory?: string;
  // Model this agent runs on (e.g. "Claude Opus 4.8"), self-reported to peers.
  model?: string;
};

export type JoinOptions = {
  // What to join: a `💬…` id. (A shared string derives its own public mesh —
  // that is `forumMesh` — and is not a join target.)
  target: string;
  // Local nickname; omit for the daemon's random `word-word`.
  nickname?: string;
  // Model this agent runs on, self-reported to peers.
  model?: string;
};

export type ForumOptions = {
  // The shared string. Hashed into a deterministic public mesh — anyone
  // passing the same string joins the same mesh.
  string: string;
  // Local nickname; omit for the daemon's random `word-word`.
  nickname?: string;
  // Model this agent runs on, self-reported to peers.
  model?: string;
};

// The harness label this extension reports to peers (the pi runtime).
const HARNESS = "pi";

// The semantic contract for create options, shared by every caller (the
// `/mesh-create` command and the mesh_create tool). Returns an error
// message, or undefined when the options are valid. The daemon stays the
// authoritative backstop; this is the single client-side source of truth.
export function validateCreateOptions(options: CreateOptions): string | undefined {
  if (options.name !== undefined && !isValidMeshName(options.name)) {
    return "invalid name — must be 1-32 chars, no whitespace or < > #";
  }
  if (options.advertise && options.network !== "public") {
    return "advertise requires public network";
  }
  return undefined;
}

// The shared tail of createMesh/joinMesh/forumMesh: spawn the daemon,
// wait for its ready event, and build the session. `timeoutMs` differs per
// caller: create is localhost-only setup, join/forum may cross the relay.
async function spawnSession({
  args,
  timeoutMs,
  model,
}: {
  args: string[];
  timeoutMs: number;
  model?: string;
}): Promise<Session> {
  cleanup();

  const filePath = stateFilePath();
  // Insert right after the subcommand, not at the end — forum's argv ends in
  // `-- <string>`, and anything appended after `--` would parse as positional.
  if (filePath) args.splice(1, 0, "--state-file", filePath);

  const child = spawn("agent-mesh", args, { stdio: ["ignore", "pipe", "pipe"] });
  // `startWatcher` attaches the single readline and resolves with the `ready`
  // line; ongoing events (incl. peers already present) flow on from the same
  // reader — no second one to drop the bundled `joined` lines.
  const readyLine = await startWatcher({ child, timeoutMs });
  const ready = JSON.parse(readyLine);

  if (!ready.mesh || !ready.name || !ready.nickname) {
    throw new Error("invalid ready event: missing mesh, name, or nickname");
  }

  if (typeof child.pid !== "number") {
    throw new Error("agent-mesh spawned without a pid");
  }
  const session: Session = {
    mesh: ready.mesh,
    name: ready.name,
    nickname: ready.nickname,
    pid: child.pid,
    drift: typeof ready.drift === "string" ? ready.drift : undefined,
  };
  state.session = session;
  reportSelfMeta(model);
  return session;
}

export async function createMesh(options: CreateOptions = {}): Promise<Session> {
  const invalid = validateCreateOptions(options);
  if (invalid) throw new Error(invalid);

  const { name, network = "private", relay, mdns, dht, advertise, directory, model } = options;

  const args = ["create", "--no-interactive", "--output", "json", "--filter-self"];
  // Omit --name entirely so the daemon mints a random name (empty is rejected by the CLI).
  if (name) args.push("--name", name);
  if (network === "public") args.push("--public");
  if (mdns) args.push("--mdns");
  if (dht) args.push("--dht");
  // Optional-value flag: `--relay=urls` for a custom ladder, bare `--relay` for the default.
  if (relay !== undefined) args.push(relay ? `--relay=${relay}` : "--relay");
  if (advertise) args.push(directory ? `--advertise=${directory}` : "--advertise");

  return spawnSession({ args, timeoutMs: 30_000, model });
}

export async function joinMesh({ target, nickname, model }: JoinOptions): Promise<Session> {
  const args = ["join", target, "--no-interactive", "--output", "json", "--filter-self"];
  if (nickname) args.push("--nickname", nickname);
  return spawnSession({ args, timeoutMs: 60_000, model });
}

export async function forumMesh({ string, nickname, model }: ForumOptions): Promise<Session> {
  const args = ["forum", "--no-interactive", "--output", "json", "--filter-self"];
  if (nickname) args.push("--nickname", nickname);
  // The string is any byte-for-byte value the peers agreed on — it may start
  // with `-`, so it goes last, after the `--` end-of-flags separator.
  args.push("--", string);
  return spawnSession({ args, timeoutMs: 60_000, model });
}

// `notice: true` sends the no-auto-reply kind (`agent-mesh notice`) — same flags,
// different receiver contract.
export function sendMeshMessage({
  text,
  reply,
  notice,
}: {
  text: string;
  reply?: string;
  notice?: boolean;
}): void {
  if (!state.session?.mesh) throw new Error("Not in a mesh");
  if (!isValidBody(text)) {
    throw new Error("Message body must not contain control characters other than tab/newline");
  }

  const args = [
    notice ? "notice" : "msg",
    "--mesh",
    state.session.mesh,
    "--nickname",
    state.session.nickname,
    "--text",
    text,
  ];
  if (reply) args.push("--reply", reply);

  runMeshCommand(args);
}

// Send one leg of a task (`agent-mesh task`). `text` is required by the CLI but may
// be empty for legs without a body (accept/confirm/cancel).
export function sendTaskLeg({
  to,
  taskId,
  phase,
  text = "",
}: {
  to: string;
  taskId: string;
  phase: string;
  text?: string;
}): void {
  if (!state.session?.mesh) throw new Error("Not in a mesh");
  runMeshCommand([
    "task",
    "--mesh",
    state.session.mesh,
    "--nickname",
    state.session.nickname,
    "--to",
    to,
    "--task-id",
    taskId,
    "--phase",
    phase,
    "--text",
    text,
  ]);
}

export function getMeshStatus(): {
  mesh: string | null;
  name: string | null;
  nickname: string | null;
} {
  return {
    mesh: state.session?.mesh ?? null,
    name: state.session?.name ?? null,
    nickname: state.session?.nickname ?? null,
  };
}

export function leaveMesh(): void {
  cleanup();
}

// Browse a directory for advertised meshes. Spawns `agent-mesh discover`, collects
// mesh_found/mesh_lost lines, then resolves: ~`graceMs` after the first hit
// (to gather a few more), or at `maxMs` if nothing shows. Discovery joins no
// mesh — the child is always killed before resolving.
export function discoverMeshes({
  directory,
  graceMs = 1500,
  maxMs = 8000,
}: {
  directory?: string;
  graceMs?: number;
  maxMs?: number;
}): Promise<DiscoveredMesh[]> {
  return new Promise((resolve) => {
    const args = ["discover", "--no-interactive", "--output", "json"];
    if (directory && directory !== "global") args.push("--directory", directory);
    const child = spawn("agent-mesh", args, { stdio: ["ignore", "pipe", "pipe"] });
    const found = new Map<string, DiscoveredMesh>();
    let graceTimer: ReturnType<typeof setTimeout> | null = null;
    let settled = false;

    const settle = () => {
      if (settled) return;
      settled = true;
      if (graceTimer) clearTimeout(graceTimer);
      clearTimeout(maxTimer);
      try {
        child.kill();
      } catch {
        /* already gone */
      }
      resolve([...found.values()].sort((left, right) => right.peers - left.peers));
    };
    const maxTimer = setTimeout(settle, maxMs);

    const stdout = child.stdout;
    if (!stdout) {
      settle();
      return;
    }
    const lineReader = readline.createInterface({ input: stdout });
    lineReader.on("line", (line) => {
      let event: {
        event?: string;
        mesh?: string;
        name?: string;
        peers?: number;
        mode?: string;
      };
      try {
        event = JSON.parse(line);
      } catch {
        return;
      }
      if (event.event === "mesh_found" && typeof event.mesh === "string") {
        found.set(event.mesh, {
          mesh: event.mesh,
          name: event.name ?? "?",
          peers: Number(event.peers ?? 0),
          mode: event.mode === "public" ? "public" : "private",
        });
        if (!graceTimer) graceTimer = setTimeout(settle, graceMs);
      } else if (event.event === "mesh_lost" && typeof event.mesh === "string") {
        found.delete(event.mesh);
      }
    });
    child.on("error", settle);
  });
}

// Query the live roster via `agent-mesh peers`. Throws when not in a mesh.
export function getPeers(): { count: number; participants: Peer[] } {
  if (!state.session?.mesh) throw new Error("Not in a mesh");
  const raw = runMeshCommand([
    "peers",
    "--mesh",
    state.session.mesh,
    "--nickname",
    state.session.nickname,
  ]);
  const parsed = JSON.parse(raw) as {
    participant_count: number;
    participants: Array<{
      nickname: string;
      last_seen_secs_ago?: number | null;
      quiet?: boolean;
      reach?: string;
    }>;
  };
  // Model/harness/host live in the `meta` channel now (`/peers/<nick>`), not the
  // roster — each agent self-reports them. Best-effort: a meta read failure
  // just leaves those columns blank, never breaks the roster.
  let peerMeta: Record<
    string,
    { model?: string; harness?: string; host?: string; status?: string }
  > = {};
  try {
    const doc = getMetaDocument();
    if (doc.peers && typeof doc.peers === "object") {
      peerMeta = doc.peers as Record<
        string,
        { model?: string; harness?: string; host?: string; status?: string }
      >;
    }
  } catch {
    // leave peerMeta empty
  }
  return {
    count: parsed.participant_count,
    participants: parsed.participants.map((entry) => ({
      nickname: entry.nickname,
      reach: entry.reach === "direct" ? "direct" : "gossip",
      model: peerMeta[entry.nickname]?.model,
      harness: peerMeta[entry.nickname]?.harness,
      host: peerMeta[entry.nickname]?.host,
      status: peerMeta[entry.nickname]?.status,
      lastSeenSecsAgo: entry.last_seen_secs_ago ?? null,
      quiet: Boolean(entry.quiet),
    })),
  };
}

// Read the current derived shared-state document via `agent-mesh state get`. Throws
// when not in a mesh. Uses execFileSync (no shell), consistent with
// applyStateMerge — its JSON `--merge` arg must never touch the shell.
export function getStateDocument(): Record<string, unknown> {
  if (!state.session?.mesh) throw new Error("Not in a mesh");
  const raw = execFileSync(
    "agent-mesh",
    ["state", "get", "--mesh", state.session.mesh, "--nickname", state.session.nickname],
    { encoding: "utf-8", timeout: 15_000 },
  ).trim();
  const parsed = JSON.parse(raw) as { ok: boolean; document?: Record<string, unknown> };
  return parsed.document ?? {};
}

// Apply an RFC 7386 JSON Merge Patch to the shared state via `agent-mesh state merge`.
// The outcome is read from the `{ok,…}` JSON on stdout. The JSON `--merge` arg
// goes through execFileSync (no shell) — `runMeshCommand`'s quoting would mangle
// it.
export function applyStateMerge({ merge }: { merge: string }): {
  ok: boolean;
  error?: string;
} {
  if (!state.session?.mesh) throw new Error("Not in a mesh");
  let parsed: unknown;
  try {
    parsed = JSON.parse(merge);
  } catch {
    throw new Error("merge must be a JSON object (RFC 7386 merge patch)");
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    throw new Error("merge must be a JSON object (RFC 7386 merge patch)");
  }

  const raw = execFileSync(
    "agent-mesh",
    [
      "state",
      "merge",
      "--mesh",
      state.session.mesh,
      "--nickname",
      state.session.nickname,
      "--merge",
      merge,
    ],
    { encoding: "utf-8", timeout: 15_000 },
  ).trim();
  const resp = JSON.parse(raw) as { ok: boolean; error?: string };
  return { ok: resp.ok, error: resp.error };
}

// Read the derived `meta`-channel document via `agent-mesh meta get`. The meta
// channel is byte-for-byte the same machinery as `state`; by convention it
// holds mesh metadata, e.g. `/peers/<nick> = { model, harness, host }`.
export function getMetaDocument(): Record<string, unknown> {
  if (!state.session?.mesh) throw new Error("Not in a mesh");
  const raw = execFileSync(
    "agent-mesh",
    ["meta", "get", "--mesh", state.session.mesh, "--nickname", state.session.nickname],
    { encoding: "utf-8", timeout: 15_000 },
  ).trim();
  const parsed = JSON.parse(raw) as { ok: boolean; document?: Record<string, unknown> };
  return parsed.document ?? {};
}

// Apply an RFC 7386 JSON Merge Patch to the `meta` channel. Like `state merge`,
// the CLI exits non-zero on a rejected `{ok:false}` merge, so execFileSync
// throws — callers that tolerate rejection wrap it.
function runMetaMerge(merge: string): void {
  if (!state.session?.mesh) throw new Error("Not in a mesh");
  execFileSync(
    "agent-mesh",
    [
      "meta",
      "merge",
      "--mesh",
      state.session.mesh,
      "--nickname",
      state.session.nickname,
      "--merge",
      merge,
    ],
    { encoding: "utf-8", timeout: 15_000 },
  );
}

// Record what this agent runs on into `meta` under `/peers/<nickname>`. The
// binary no longer self-reports model/harness/host — it is mesh metadata the
// agent owns. Best-effort: a failed report must not break create/join, so errors
// are swallowed (the roster simply shows no model for us). A merge deep-merges
// only our own `/peers/<nickname>` key, so it creates `/peers` if absent and
// never clobbers another peer's entry — no seed, no fallback needed.
function reportSelfMeta(model?: string): void {
  const session = state.session;
  if (!session?.mesh) return;
  // Seed `status: "idle"` — we advertise as accepting work until the agent flips
  // it (via setSelfStatus) when it goes heads-down. Only "busy" makes senders
  // skip us; "idle"/"available"/absent stay eligible.
  const value: Record<string, string> = { harness: HARNESS, status: "idle" };
  if (model) value.model = model;
  // `host` is this machine's short hostname, so peers can see which agents
  // share a box. os.hostname() may include a domain — keep only the first label.
  const host = hostname().split(".")[0];
  if (host) value.host = host;
  const merge = JSON.stringify({ peers: { [session.nickname]: value } });
  try {
    runMetaMerge(merge);
  } catch {
    // Give up silently; self-report is non-essential.
  }
}

// Advertise this agent's availability into `meta` under `/peers/<nickname>/status`.
// A partial merge touches only `status`, leaving model/harness/host intact.
// Throws on a rejected merge so the tool can surface the error to the agent.
export function setSelfStatus(status: string): void {
  const session = state.session;
  if (!session?.mesh) throw new Error("Not in a mesh");
  runMetaMerge(JSON.stringify({ peers: { [session.nickname]: { status } } }));
}

export async function pingPeers(): Promise<PingResult[]> {
  if (!state.session?.mesh) throw new Error("Not in a mesh");

  state.pingPending = true;
  state.pingStartTime = Date.now();
  state.pongMap.clear();

  try {
    runMeshCommand([
      "msg",
      "--mesh",
      state.session.mesh,
      "--nickname",
      state.session.nickname,
      "--text",
      "ping",
    ]);
  } catch (error) {
    state.pingPending = false;
    throw new Error(`Ping failed: ${error instanceof Error ? error.message : "unknown"}`);
  }

  await new Promise((resolve) => setTimeout(resolve, 10_000));
  state.pingPending = false;

  const results: PingResult[] = [];
  for (const [author, rtt] of state.pongMap) {
    results.push({ author, rtt });
  }
  return results;
}
