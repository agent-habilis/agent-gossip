import type { ChildProcess } from "node:child_process";
import { spawn } from "node:child_process";
import * as readline from "node:readline";
import { clearBatch, startWatcher, stopWatcher } from "./daemon";
import { isAscii, runSwarmCommand } from "./helpers";
import { state, stateFilePath } from "./state";
import type { PingResult, Session } from "./types";

function waitForReady(child: ChildProcess, timeoutMs: number): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const stdout = child.stdout;
    if (!stdout) {
      reject(new Error("agent-habilis-swarm spawned without a stdout stream"));
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

export async function createSwarm(
  name: string,
  network = "private",
  relay?: string,
): Promise<Session> {
  cleanup();

  const args = ["create", "--name", name, "--no-interactive", "--output", "json", "--filter-self"];
  if (network === "public") args.push("--network", "public");
  if (relay) args.push("--relay", relay);
  const filePath = stateFilePath();
  if (filePath) args.push("--state-file", filePath);

  const child = spawn("agent-habilis-swarm", args, { stdio: ["ignore", "pipe", "pipe"] });
  const readyLine = await waitForReady(child, 30_000);
  const ready = JSON.parse(readyLine);

  if (!ready.swarm || !ready.name || !ready.nickname) {
    throw new Error("invalid ready event: missing swarm, name, or nickname");
  }

  if (typeof child.pid !== "number") {
    throw new Error("agent-habilis-swarm spawned without a pid");
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

  const child = spawn("agent-habilis-swarm", args, { stdio: ["ignore", "pipe", "pipe"] });
  const readyLine = await waitForReady(child, 60_000);
  const ready = JSON.parse(readyLine);

  if (!ready.swarm || !ready.name || !ready.nickname) {
    throw new Error("invalid ready event: missing swarm, name, or nickname");
  }

  if (typeof child.pid !== "number") {
    throw new Error("agent-habilis-swarm spawned without a pid");
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
  if (!isAscii(text)) throw new Error("Message body must be ASCII only");

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
