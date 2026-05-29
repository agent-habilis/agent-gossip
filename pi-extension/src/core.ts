import type { ChildProcess } from "node:child_process";
import { spawn } from "node:child_process";
import * as readline from "node:readline";
import { clearBatch, startWatcher, stopWatcher } from "./daemon";
import { isValidBody, isValidSwarmName, runSwarmCommand } from "./helpers";
import { state, stateFilePath } from "./state";
import type { PingResult, Session } from "./types";

function waitForReady(child: ChildProcess, timeoutMs: number): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const stdout = child.stdout;
    if (!stdout) {
      reject(new Error("ahs spawned without a stdout stream"));
      return;
    }
    const lineReader = readline.createInterface({ input: stdout });
    const timeout = setTimeout(() => {
      lineReader.close();
      child.kill();
      reject(new Error(`timeout waiting for ready event (${timeoutMs}ms)`));
    }, timeoutMs);

    lineReader.on("line", (line) => {
      if (line.includes('"event":"ready"')) {
        clearTimeout(timeout);
        lineReader.close();
        resolve(line);
      }
    });
    child.on("error", (error) => {
      clearTimeout(timeout);
      lineReader.close();
      reject(new Error(`spawn failed: ${error.message}`));
    });
  });
}

export function cleanup(): void {
  stopWatcher();
  clearBatch();
  state.session = null;
  state.pendingMessages = [];
  state.pingPending = false;
  state.pongMap.clear();
}

export type CreateOptions = {
  // Omit/empty ⇒ the daemon mints a random `word-word` name.
  name?: string;
  network?: "private" | "public";
  // undefined ⇒ relay off; "" ⇒ default n0 ladder; "a,b" ⇒ custom ladder.
  relay?: string;
  mdns?: boolean;
  dht?: boolean;
  // Per-author messages-per-minute cap baked into the id. 0 disables.
  rateLimit?: number;
  // List the swarm in a directory; requires network "public".
  advertise?: boolean;
  // Directory to advertise into; omit for the well-known `global`.
  directory?: string;
};

// The semantic contract for create options, shared by every caller (the
// `/swarm-create` command and the swarm_create tool). Returns an error
// message, or undefined when the options are valid. The daemon stays the
// authoritative backstop; this is the single client-side source of truth.
export function validateCreateOptions(options: CreateOptions): string | undefined {
  if (options.name !== undefined && !isValidSwarmName(options.name)) {
    return "invalid name — must be 1-32 chars, no whitespace or / \\ < > #";
  }
  if (
    options.rateLimit !== undefined &&
    (!Number.isInteger(options.rateLimit) || options.rateLimit < 0)
  ) {
    return `invalid rate limit: ${options.rateLimit}`;
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

  const { name, network = "private", relay, mdns, dht, rateLimit, advertise, directory } = options;

  const args = ["create", "--no-interactive", "--output", "json", "--filter-self"];
  // Omit --name entirely so the daemon mints a random name (empty is rejected by the CLI).
  if (name) args.push("--name", name);
  if (network === "public") args.push("--public");
  if (mdns) args.push("--mdns");
  if (dht) args.push("--dht");
  // Optional-value flag: `--relay=urls` for a custom ladder, bare `--relay` for the default.
  if (relay !== undefined) args.push(relay ? `--relay=${relay}` : "--relay");
  if (rateLimit !== undefined) args.push("--rate-limit", String(rateLimit));
  if (advertise) args.push(directory ? `--advertise=${directory}` : "--advertise");
  const filePath = stateFilePath();
  if (filePath) args.push("--state-file", filePath);

  const child = spawn("ahs", args, { stdio: ["ignore", "pipe", "pipe"] });
  const readyLine = await waitForReady(child, 30_000);
  const ready = JSON.parse(readyLine);

  if (!ready.swarm || !ready.name || !ready.nickname) {
    throw new Error("invalid ready event: missing swarm, name, or nickname");
  }

  if (typeof child.pid !== "number") {
    throw new Error("ahs spawned without a pid");
  }
  const session: Session = {
    swarm: ready.swarm,
    name: ready.name,
    nickname: ready.nickname,
    pid: child.pid,
  };
  state.session = session;
  startWatcher(child);
  return session;
}

export async function joinSwarm(target: string, nickname?: string): Promise<Session> {
  cleanup();

  const args = ["join", target, "--no-interactive", "--output", "json", "--filter-self"];
  if (nickname) args.push("--nickname", nickname);
  const filePath = stateFilePath();
  if (filePath) args.push("--state-file", filePath);

  const child = spawn("ahs", args, { stdio: ["ignore", "pipe", "pipe"] });
  const readyLine = await waitForReady(child, 60_000);
  const ready = JSON.parse(readyLine);

  if (!ready.swarm || !ready.name || !ready.nickname) {
    throw new Error("invalid ready event: missing swarm, name, or nickname");
  }

  if (typeof child.pid !== "number") {
    throw new Error("ahs spawned without a pid");
  }
  const session: Session = {
    swarm: ready.swarm,
    name: ready.name,
    nickname: ready.nickname,
    pid: child.pid,
  };
  state.session = session;
  startWatcher(child);
  return session;
}

export function sendSwarmMessage(text: string, reply?: string): void {
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

export function getSwarmStatus(): {
  swarm: string | null;
  name: string | null;
  nickname: string | null;
  autoReply: boolean;
} {
  return {
    swarm: state.session?.swarm ?? null,
    name: state.session?.name ?? null,
    nickname: state.session?.nickname ?? null,
    autoReply: state.autoReply,
  };
}

export function leaveSwarm(): void {
  cleanup();
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
