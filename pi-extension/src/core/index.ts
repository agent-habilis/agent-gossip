import { execFileSync, spawn } from "node:child_process";
import * as readline from "node:readline";
import { clearBatch, startWatcher, stopWatcher } from "../daemon";
import { isValidBody, isValidSwarmName, runSwarmCommand } from "../helpers";
import { state, stateFilePath } from "../state";
import type { DiscoveredSwarm, ExchangeKind, Peer, PingResult, Session } from "../types";

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
  // List the swarm in a directory; requires network "public".
  advertise?: boolean;
  // Directory to advertise into; omit for the well-known `global`.
  directory?: string;
  // Model this agent runs on (e.g. "Claude Opus 4.8"), self-reported to peers.
  model?: string;
};

export type JoinOptions = {
  // What to join: a `🐝…` id, a domain, or a supported git repo URL.
  target: string;
  // Local nickname; omit for the daemon's random `word-word`.
  nickname?: string;
  // Model this agent runs on, self-reported to peers.
  model?: string;
};

// The harness label this extension reports to peers (the pi runtime).
const HARNESS = "pi";

// The semantic contract for create options, shared by every caller (the
// `/swarm-create` command and the swarm_create tool). Returns an error
// message, or undefined when the options are valid. The daemon stays the
// authoritative backstop; this is the single client-side source of truth.
export function validateCreateOptions(options: CreateOptions): string | undefined {
  if (options.name !== undefined && !isValidSwarmName(options.name)) {
    return "invalid name — must be 1-32 chars, no whitespace or / \\ < > #";
  }
  if (options.advertise && options.network !== "public") {
    return "advertise requires public network";
  }
  return undefined;
}

export async function createSwarm(options: CreateOptions = {}): Promise<Session> {
  const invalid = validateCreateOptions(options);
  if (invalid) throw new Error(invalid);

  cleanup();

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
  const filePath = stateFilePath();
  if (filePath) args.push("--state-file", filePath);

  const child = spawn("ahsw", args, { stdio: ["ignore", "pipe", "pipe"] });
  // `startWatcher` attaches the single readline and resolves with the `ready`
  // line; ongoing events (incl. peers already present) flow on from the same
  // reader — no second one to drop the bundled `joined` lines.
  const readyLine = await startWatcher({ child, timeoutMs: 30_000 });
  const ready = JSON.parse(readyLine);

  if (!ready.swarm || !ready.name || !ready.nickname) {
    throw new Error("invalid ready event: missing swarm, name, or nickname");
  }

  if (typeof child.pid !== "number") {
    throw new Error("ahsw spawned without a pid");
  }
  const session: Session = {
    swarm: ready.swarm,
    name: ready.name,
    nickname: ready.nickname,
    pid: child.pid,
    drift: typeof ready.drift === "string" ? ready.drift : undefined,
  };
  state.session = session;
  reportSelfMeta(model);
  return session;
}

export async function joinSwarm({ target, nickname, model }: JoinOptions): Promise<Session> {
  cleanup();

  const args = ["join", target, "--no-interactive", "--output", "json", "--filter-self"];
  if (nickname) args.push("--nickname", nickname);
  const filePath = stateFilePath();
  if (filePath) args.push("--state-file", filePath);

  const child = spawn("ahsw", args, { stdio: ["ignore", "pipe", "pipe"] });
  const readyLine = await startWatcher({ child, timeoutMs: 60_000 });
  const ready = JSON.parse(readyLine);

  if (!ready.swarm || !ready.name || !ready.nickname) {
    throw new Error("invalid ready event: missing swarm, name, or nickname");
  }

  if (typeof child.pid !== "number") {
    throw new Error("ahsw spawned without a pid");
  }
  const session: Session = {
    swarm: ready.swarm,
    name: ready.name,
    nickname: ready.nickname,
    pid: child.pid,
    drift: typeof ready.drift === "string" ? ready.drift : undefined,
  };
  state.session = session;
  reportSelfMeta(model);
  return session;
}

export function sendSwarmMessage({ text, reply }: { text: string; reply?: string }): void {
  if (!state.session?.swarm) throw new Error("Not in a swarm");
  if (!isValidBody(text)) {
    throw new Error("Message body must not contain control characters other than tab/newline");
  }

  const args = [
    "msg",
    "--swarm",
    state.session.swarm,
    "--nickname",
    state.session.nickname,
    "--text",
    text,
  ];
  if (reply) args.push("--reply", reply);

  runSwarmCommand(args);
}

// Send one leg of an exchange (`ahsw exchange`). `text` is required by the CLI
// but may be empty for legs without a body (accept/confirm/cancel).
export function sendExchange({
  to,
  exchangeId,
  kind,
  phase,
  text = "",
}: {
  to: string;
  exchangeId: string;
  kind: ExchangeKind;
  phase: string;
  text?: string;
}): void {
  if (!state.session?.swarm) throw new Error("Not in a swarm");
  runSwarmCommand([
    "exchange",
    "--swarm",
    state.session.swarm,
    "--nickname",
    state.session.nickname,
    "--to",
    to,
    "--exchange-id",
    exchangeId,
    "--kind",
    kind,
    "--phase",
    phase,
    "--text",
    text,
  ]);
}

export function getSwarmStatus(): {
  swarm: string | null;
  name: string | null;
  nickname: string | null;
} {
  return {
    swarm: state.session?.swarm ?? null,
    name: state.session?.name ?? null,
    nickname: state.session?.nickname ?? null,
  };
}

export function leaveSwarm(): void {
  cleanup();
}

// Browse a directory for advertised swarms. Spawns `ahsw discover`, collects
// swarm_found/swarm_lost lines, then resolves: ~`graceMs` after the first hit
// (to gather a few more), or at `maxMs` if nothing shows. Discovery joins no
// swarm — the child is always killed before resolving.
export function discoverSwarms({
  directory,
  graceMs = 1500,
  maxMs = 8000,
}: {
  directory?: string;
  graceMs?: number;
  maxMs?: number;
}): Promise<DiscoveredSwarm[]> {
  return new Promise((resolve) => {
    const args = ["discover", "--no-interactive", "--output", "json"];
    if (directory && directory !== "global") args.push("--directory", directory);
    const child = spawn("ahsw", args, { stdio: ["ignore", "pipe", "pipe"] });
    const found = new Map<string, DiscoveredSwarm>();
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
        swarm?: string;
        name?: string;
        peers?: number;
        mode?: string;
      };
      try {
        event = JSON.parse(line);
      } catch {
        return;
      }
      if (event.event === "swarm_found" && typeof event.swarm === "string") {
        found.set(event.swarm, {
          swarm: event.swarm,
          name: event.name ?? "?",
          peers: Number(event.peers ?? 0),
          mode: event.mode === "public" ? "public" : "private",
        });
        if (!graceTimer) graceTimer = setTimeout(settle, graceMs);
      } else if (event.event === "swarm_lost" && typeof event.swarm === "string") {
        found.delete(event.swarm);
      }
    });
    child.on("error", settle);
  });
}

// Query the live roster via `ahsw peers`. Throws when not in a swarm.
export function getPeers(): { count: number; participants: Peer[] } {
  if (!state.session?.swarm) throw new Error("Not in a swarm");
  const raw = runSwarmCommand([
    "peers",
    "--swarm",
    state.session.swarm,
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
  // Model/harness live in the `meta` channel now (`/peers/<nick>`), not the
  // roster — each agent self-reports them. Best-effort: a meta read failure
  // just leaves those columns blank, never breaks the roster.
  let peerMeta: Record<string, { model?: string; harness?: string }> = {};
  try {
    const doc = getMetaDocument();
    if (doc.peers && typeof doc.peers === "object") {
      peerMeta = doc.peers as Record<string, { model?: string; harness?: string }>;
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
      lastSeenSecsAgo: entry.last_seen_secs_ago ?? null,
      quiet: Boolean(entry.quiet),
    })),
  };
}

// Read the current derived shared-state document via `ahsw state get`. Throws
// when not in a swarm. Uses execFileSync (no shell), consistent with
// applyStatePatch — its JSON `--patch` arg must never touch the shell.
export function getStateDocument(): Record<string, unknown> {
  if (!state.session?.swarm) throw new Error("Not in a swarm");
  const raw = execFileSync(
    "ahsw",
    ["state", "get", "--swarm", state.session.swarm, "--nickname", state.session.nickname],
    { encoding: "utf-8", timeout: 15_000 },
  ).trim();
  const parsed = JSON.parse(raw) as { ok: boolean; document?: Record<string, unknown> };
  return parsed.document ?? {};
}

// Apply an RFC 6902 patch to the shared state via `ahsw state patch`. The
// outcome is read from the `{ok,…}` JSON on stdout. The JSON `--patch` arg goes
// through execFileSync (no shell) — `runSwarmCommand`'s quoting would mangle it.
export function applyStatePatch({ patch }: { patch: string }): {
  ok: boolean;
  error?: string;
} {
  if (!state.session?.swarm) throw new Error("Not in a swarm");
  let parsed: unknown;
  try {
    parsed = JSON.parse(patch);
  } catch {
    throw new Error("patch must be a JSON array of RFC 6902 ops");
  }
  if (!Array.isArray(parsed)) throw new Error("patch must be a JSON array of RFC 6902 ops");

  const raw = execFileSync(
    "ahsw",
    [
      "state",
      "patch",
      "--swarm",
      state.session.swarm,
      "--nickname",
      state.session.nickname,
      "--patch",
      patch,
    ],
    { encoding: "utf-8", timeout: 15_000 },
  ).trim();
  const resp = JSON.parse(raw) as { ok: boolean; error?: string };
  return { ok: resp.ok, error: resp.error };
}

// Read the derived `meta`-channel document via `ahsw meta get`. The meta
// channel is byte-for-byte the same machinery as `state`; by convention it
// holds swarm metadata, e.g. `/peers/<nick> = { model, harness }`.
export function getMetaDocument(): Record<string, unknown> {
  if (!state.session?.swarm) throw new Error("Not in a swarm");
  const raw = execFileSync(
    "ahsw",
    ["meta", "get", "--swarm", state.session.swarm, "--nickname", state.session.nickname],
    { encoding: "utf-8", timeout: 15_000 },
  ).trim();
  const parsed = JSON.parse(raw) as { ok: boolean; document?: Record<string, unknown> };
  return parsed.document ?? {};
}

// Apply an RFC 6902 patch to the `meta` channel. Like `state patch`, the CLI
// exits non-zero on a rejected `{ok:false}` patch, so execFileSync throws —
// callers that tolerate rejection (e.g. reportSelfMeta's seed fallback) wrap it.
function runMetaPatch(patch: string): void {
  if (!state.session?.swarm) throw new Error("Not in a swarm");
  execFileSync(
    "ahsw",
    [
      "meta",
      "patch",
      "--swarm",
      state.session.swarm,
      "--nickname",
      state.session.nickname,
      "--patch",
      patch,
    ],
    { encoding: "utf-8", timeout: 15_000 },
  );
}

// Record what this agent runs on into `meta` under `/peers/<nickname>`. The
// binary no longer self-reports model/harness — it is swarm metadata the agent
// owns. Best-effort: a failed report must not break create/join, so errors are
// swallowed (the roster simply shows no model for us). `/peers` is an object
// keyed by nickname so each peer owns its own path and never clobbers another's.
function reportSelfMeta(model?: string): void {
  const session = state.session;
  if (!session?.swarm) return;
  const value: Record<string, string> = { harness: HARNESS };
  if (model) value.model = model;
  const entry = JSON.stringify(value);
  const nick = JSON.stringify(session.nickname);
  try {
    // The common case: /peers already exists (the creator seeded it).
    runMetaPatch(`[{"op":"add","path":"/peers/${session.nickname}","value":${entry}}]`);
  } catch {
    // /peers not present yet (sole or early member) — create it with our entry.
    try {
      runMetaPatch(`[{"op":"add","path":"/peers","value":{${nick}:${entry}}}]`);
    } catch {
      // Give up silently; self-report is non-essential.
    }
  }
}

export async function pingPeers(): Promise<PingResult[]> {
  if (!state.session?.swarm) throw new Error("Not in a swarm");

  state.pingPending = true;
  state.pingStartTime = Date.now();
  state.pongMap.clear();

  try {
    runSwarmCommand([
      "msg",
      "--swarm",
      state.session.swarm,
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
