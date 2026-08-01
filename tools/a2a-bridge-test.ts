#!/usr/bin/env bun
// End-to-end a2a bridge test: bridge two vanilla A2A agents over the mesh.
//
//   agent A (server) ──▶ agent-gossip a2a expose ──mesh──▶ agent-gossip a2a connect ──▶ agent B (client)
//
// Agent B discovers agent A purely through the bridge (the Agent Card's
// rewritten `url`) and exchanges a message with the raw A2A protocol. Both
// `agent-gossip` sides run with `--loopback`, so the whole thing is hermetic on one
// machine — the ticket carries the exposer's direct 127.0.0.1 address and no
// mDNS/DHT/relay is used.
//
// Usage: bun tools/a2a-bridge-test.ts   (or ./tools/a2a-bridge-test.ts)

import { dirname, join } from "node:path";

const REPO_ROOT = dirname(import.meta.dir);
const AGENT = join(REPO_ROOT, "tools", "a2a-agent", "agent.ts");
const TEXT = "hello over the mesh";

/** Invoke the agent-gossip binary through cargo — it resolves the debug build itself,
 * so there is no hardcoded target/ path to keep in sync. */
const agentRoom = (args: string[]): string[] => [
  "cargo",
  "run",
  "--quiet",
  "--bin",
  "agent-gossip",
  "--",
  ...args,
];

const children: Bun.Subprocess[] = [];
function cleanup(): void {
  for (const child of children) {
    try {
      child.kill();
    } catch {
      // already gone
    }
  }
}
process.on("exit", cleanup);
process.on("SIGINT", () => {
  cleanup();
  process.exit(130);
});

/** An ephemeral free TCP port on loopback. */
function freePort(): number {
  const listener = Bun.listen({
    hostname: "127.0.0.1",
    port: 0,
    socket: { data() {} },
  });
  const { port } = listener;
  listener.stop();
  return port;
}

/** Poll a URL until it answers, or throw after ~20s. */
async function waitForUrl(url: string, tries = 100): Promise<void> {
  for (let attempt = 0; attempt < tries; attempt++) {
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // not up yet
    }
    await Bun.sleep(200);
  }
  throw new Error(`timed out waiting for ${url}`);
}

/**
 * Read a spawned process's stdout until `expose` announces its ticket.
 *
 * Anchored on the `a2a connect <ticket>` line `expose` prints rather than on
 * the ticket's own shape: tickets are bare Base58Check with no sigil, so there
 * is nothing in the token itself to match. An earlier version scanned for a
 * `📡` brand and silently rotted when the brand changed, then vanished.
 */
async function readTicket(
  stream: ReadableStream<Uint8Array>,
  timeoutMs = 20_000,
): Promise<string> {
  const reader = stream.getReader();
  const decoder = new TextDecoder();
  const deadline = Date.now() + timeoutMs;
  let buffer = "";
  try {
    while (Date.now() < deadline) {
      const chunk = await Promise.race([
        reader.read(),
        Bun.sleep(timeoutMs).then(() => ({ done: true, value: undefined }) as const),
      ]);
      if (chunk.done) break;
      buffer += decoder.decode(chunk.value, { stream: true });
      const match = buffer.match(/a2a connect (\S+)/);
      if (match) return match[1];
    }
  } finally {
    reader.releaseLock();
  }
  throw new Error("never got a ticket from expose");
}

function run(label: string): void {
  console.log(`==> ${label}`);
}

async function main(): Promise<void> {
  run("building agent-gossip");
  const build = Bun.spawnSync(["cargo", "build", "--quiet", "--bin", "agent-gossip"], {
    cwd: REPO_ROOT,
    stdout: "inherit",
    stderr: "inherit",
  });
  if (build.exitCode !== 0) throw new Error("cargo build failed");

  const originPort = freePort();
  const localPort = freePort();

  run(`starting agent A (A2A echo server) on 127.0.0.1:${originPort}`);
  const server = Bun.spawn(
    ["bun", AGENT, "server", "--port", String(originPort), "--name", "origin-agent"],
    { stdout: "inherit", stderr: "inherit" },
  );
  children.push(server);
  await waitForUrl(`http://127.0.0.1:${originPort}/.well-known/agent-card.json`);

  run("exposing agent A over the mesh (agent-gossip a2a expose --loopback)");
  const expose = Bun.spawn(
    agentRoom(["a2a", "expose", "--to", `http://127.0.0.1:${originPort}`, "--loopback"]),
    { cwd: REPO_ROOT, stdout: "pipe", stderr: "ignore" },
  );
  children.push(expose);
  const ticket = await readTicket(expose.stdout as ReadableStream<Uint8Array>);
  console.log(`    ticket: ${ticket.slice(0, 24)}…`);

  run(`connecting from the other end (agent-gossip a2a connect --port ${localPort})`);
  const connect = Bun.spawn(
    agentRoom(["a2a", "connect", ticket, "--port", String(localPort)]),
    { cwd: REPO_ROOT, stdout: "ignore", stderr: "ignore" },
  );
  children.push(connect);
  await waitForUrl(`http://127.0.0.1:${localPort}/.well-known/agent-card.json`);

  run("agent B (A2A client) talks to agent A through the local bridge");
  const client = Bun.spawnSync(
    ["bun", AGENT, "client", "--url", `http://127.0.0.1:${localPort}`, "--text", TEXT],
    { stdout: "pipe", stderr: "pipe" },
  );
  const output = client.stdout.toString();
  for (const line of output.trimEnd().split("\n")) console.log(`    ${line}`);

  // Prove the card was rewritten: agent B must have discovered the *local*
  // bridge endpoint, not agent A's origin port.
  if (!output.includes(`endpoint http://127.0.0.1:${localPort}/`)) {
    throw new Error("the Agent Card url was not rewritten to the local bridge");
  }
  if (!output.includes("OK:")) {
    throw new Error(`the A2A round-trip did not succeed\n${client.stderr.toString()}`);
  }

  run("PASS: two vanilla A2A agents communicated over the mesh bridge");
}

try {
  await main();
  process.exit(0);
} catch (error) {
  console.error(`FAIL: ${error instanceof Error ? error.message : error}`);
  process.exit(1);
}
