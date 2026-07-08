import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  ClientFactory,
  ClientFactoryOptions,
  DefaultAgentCardResolver,
  JsonRpcTransportFactory,
} from "@a2a-js/sdk/client";
import type { Message, Part } from "@a2a-js/sdk";

// The follow-up text the initiator sends into the task after reviewing the
// artifact; the worker watches the task history for it before completing.
export const APPROVAL_TEXT = "approve";

export interface DaemonSession {
  square: string;
  name: string;
  nickname: string;
  a2aPort: number;
  a2aToken: string;
}

export function agentSquareBin(): string {
  return (
    process.env.AGENT_SQUARE_BIN ??
    new URL("../../target/debug/agent-square", import.meta.url).pathname
  );
}

// The daemon writes `--state-file` atomically and flips `ready` once its
// event loop serves IPC; `a2a_port`/`a2a_token` appear with `--a2a-serve`.
export async function waitForSession(
  stateFile: string,
  timeoutMs = 30_000,
): Promise<DaemonSession> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const raw = await Bun.file(stateFile).json();
      if (raw.ready && raw.a2a_port && raw.a2a_token) {
        return {
          square: raw.square,
          name: raw.name,
          nickname: raw.nickname,
          a2aPort: raw.a2a_port,
          a2aToken: raw.a2a_token,
        };
      }
    } catch {
      // not written yet, or mid-rename — keep polling
    }
    await Bun.sleep(150);
  }
  throw new Error(`daemon state file ${stateFile} not ready after ${timeoutMs}ms`);
}

// Every RPC on the localhost binding requires the per-daemon bearer token
// (the card endpoints do not, but sending it there is harmless).
export function makeClientFactory(token: string): ClientFactory {
  const fetchImpl: typeof fetch = (input, init = {}) => {
    const headers = new Headers(init.headers);
    headers.set("authorization", `Bearer ${token}`);
    return fetch(input, { ...init, headers });
  };
  const options = ClientFactoryOptions.createFrom(ClientFactoryOptions.default, {
    transports: [new JsonRpcTransportFactory({ fetchImpl })],
    cardResolver: new DefaultAgentCardResolver({ fetchImpl }),
  });
  return new ClientFactory(options);
}

export function messageText(message: Message | undefined): string {
  if (!message) return "";
  return message.parts
    .map((part: Part) => (part.content?.$case === "text" ? part.content.value : ""))
    .filter(Boolean)
    .join("\n");
}

export async function runAgentSquare(args: string[]): Promise<void> {
  const proc = Bun.spawn([agentSquareBin(), ...args], {
    stdout: "inherit",
    stderr: "inherit",
  });
  const code = await proc.exited;
  if (code !== 0) {
    throw new Error(`agent-square ${args.join(" ")} exited with ${code}`);
  }
}

export interface Mesh {
  dir: string;
  stateA: string;
  stateB: string;
  sessionA: DaemonSession;
  sessionB: DaemonSession;
  daemonALogPath: string;
  stop(): void;
}

// Two agent-square daemons on a fresh loopback square: daemon A (`create`)
// and daemon B (`join`, nicknamed "worker"), both with `--a2a-serve`. A
// fresh mkdtempSync dir + OS-assigned ports every call, so this is safe to
// invoke repeatedly with no leftover state between runs.
export async function startMesh(): Promise<Mesh> {
  const bin = agentSquareBin();
  const dir = mkdtempSync(join(tmpdir(), "a2a-interop-"));
  const stateA = join(dir, "a.json");
  const stateB = join(dir, "b.json");
  const daemonALogPath = join(dir, "daemon-a.log");

  const daemonA = Bun.spawn(
    [bin, "create", "--name", "a2a-interop", "--no-interactive", "--output", "json", "--a2a-serve", "--state-file", stateA],
    { stdout: Bun.file(daemonALogPath), stderr: Bun.file(join(dir, "daemon-a.err.log")) },
  );
  const children = [daemonA];
  const stop = () => {
    for (const child of children) child.kill();
  };

  try {
    const sessionA = await waitForSession(stateA);

    const daemonB = Bun.spawn(
      [bin, "join", sessionA.square, "--nickname", "worker", "--no-interactive", "--output", "json", "--a2a-serve", "--state-file", stateB],
      { stdout: Bun.file(join(dir, "daemon-b.log")), stderr: Bun.file(join(dir, "daemon-b.err.log")) },
    );
    children.push(daemonB);
    const sessionB = await waitForSession(stateB);

    return { dir, stateA, stateB, sessionA, sessionB, daemonALogPath, stop };
  } catch (error) {
    stop();
    throw error;
  }
}
