#!/usr/bin/env bun
// End-to-end check: three real `agent-gossip` processes on one loopback mesh.
//
// Proves the migration end to end, not just that it links:
//   1. three separate OS processes mesh and see each other
//   2. broadcast reaches every peer
//   3. a directed frame reaches its addressee and NOT a bystander
//      (the 0b9f438 fix, hand-ported onto a diverged tree in Phase 1)
//   4. an artifact rides the blob-offload side-channel and is fetched back
//      (the ops::blob code restored in Phase 2 — its only e2e exercise)

import { type SpawnSyncReturns, spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

type Dict = Record<string, unknown>;

const BIN = process.env.BIN;
if (!BIN) {
  console.error("BIN: set BIN to the agent-gossip binary");
  process.exit(1);
}

const NICKS = ["alice", "bob", "carol"] as const;
const RUN = fs.mkdtempSync(path.join(os.tmpdir(), "e2e-migration-"));
let failed = false;

// Ends the run early. Cleanup still stops the daemons.
class Abort extends Error {}

const out = (text: string): void => {
  process.stdout.write(text);
};
const log = (msg: string): void => out(`\n\x1b[1m== ${msg}\x1b[0m\n`);
const pass = (msg: string): void => out(`  \x1b[32mPASS\x1b[0m ${msg}\n`);
const fail = (msg: string): void => {
  out(`  \x1b[31mFAIL\x1b[0m ${msg}\n`);
  failed = true;
};

const sleep = (ms: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, ms));

const readText = (file: string): string => {
  try {
    return fs.readFileSync(file, "utf8");
  } catch {
    return "";
  }
};

const firstLine = (text: string, needle: string): string =>
  text.split("\n").find((line) => line.includes(needle)) ?? "";

const headBytes = (text: string, max: number): string =>
  Buffer.from(text).subarray(0, max).toString();

// `sed -n '1,40p' a b` counts lines across all its inputs, so it prints 40 lines in total.
const printHead = (files: string[]): void => {
  const text = files.map(readText).join("");
  if (text === "") return;
  const body = text.endsWith("\n") ? text.slice(0, -1) : text;
  for (const line of body.split("\n").slice(0, 40)) out(`${line}\n`);
};

const isDict = (value: unknown): value is Dict =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const dict = (value: unknown): Dict | undefined =>
  isDict(value) ? value : undefined;

// Keeps the value after the last `"gossip":"` key. Without a match, keeps the line.
const gossipFrom = (line: string): string => {
  const match = /^.*"gossip":"([^"]+)"/.exec(line);
  return match ? match[1] : line;
};

const waitFor = async (
  needle: string,
  file: string,
  secs = 30,
): Promise<boolean> => {
  for (let i = 0; i < secs * 4; i++) {
    if (readText(file).includes(needle)) return true;
    await sleep(250);
  }
  return false;
};

// `start` runs one daemon in the background. Its pid goes to a file, so cleanup can find it.
const start = (args: string[], nick: string): void => {
  const stdout = fs.openSync(`${RUN}/${nick}.log`, "w");
  const stderr = fs.openSync(`${RUN}/${nick}.err`, "w");
  const child = spawn(BIN, args, { stdio: ["ignore", stdout, stderr] });
  child.unref();
  fs.closeSync(stdout);
  fs.closeSync(stderr);
  fs.writeFileSync(`${RUN}/${nick}.pid`, `${child.pid}\n`);
};

type Run = { status: number | null; stdout: string; combined: string };

// Runs one short-lived `agent-gossip` command and waits for it.
const exec = (args: string[]): Run => {
  const result: SpawnSyncReturns<string> = spawnSync(BIN, args, {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  const stdout = result.stdout ?? "";
  const stderr = result.stderr ?? "";
  return { status: result.status, stdout, combined: stdout + stderr };
};

const signalDaemons = (signal: NodeJS.Signals): void => {
  for (const nick of NICKS) {
    const pidFile = `${RUN}/${nick}.pid`;
    if (!fs.existsSync(pidFile)) continue;
    const pid = Number(readText(pidFile).trim());
    // pid 0 would signal this process group, so refuse anything that is not a real pid.
    if (!Number.isInteger(pid) || pid <= 0) continue;
    try {
      process.kill(pid, signal);
    } catch {
      // The daemon already exited.
    }
  }
};

const cleanup = async (): Promise<void> => {
  signalDaemons("SIGTERM");
  await sleep(1000);
  signalDaemons("SIGKILL");
};

const parseLines = (file: string): unknown[] => {
  const docs: unknown[] = [];
  for (const line of readText(file).split("\n")) {
    try {
      docs.push(JSON.parse(line));
    } catch {
      // Not a JSON line.
    }
  }
  return docs;
};

const findTaskId = (file: string): string => {
  for (const doc of parseLines(file)) {
    if (!isDict(doc)) continue;
    const task = dict(dict(doc.result)?.task);
    if (task?.id) return String(task.id);
  }
  return "";
};

// The artifact body is sealed on the wire and comes back as JSON nested inside a
// JSON string. Walk the whole structure, re-parsing embedded strings, and read the
// `url` from the artifact part only. The agent cards also carry `url` keys, and
// those are key material, not tickets.
const artifactUrls = (node: unknown): string[] => {
  if (typeof node === "string") {
    if (!node.startsWith("{") && !node.startsWith("[")) return [];
    try {
      return artifactUrls(JSON.parse(node));
    } catch {
      return [];
    }
  }
  if (Array.isArray(node)) return node.flatMap(artifactUrls);
  if (!isDict(node)) return [];
  const urls: string[] = [];
  const parts = dict(dict(node.payload)?.artifact)?.parts;
  if (Array.isArray(parts)) {
    for (const part of parts) {
      const url = dict(part)?.url;
      if (typeof url === "string" && url) urls.push(url);
    }
  }
  for (const value of Object.values(node)) urls.push(...artifactUrls(value));
  return urls;
};

const findBlobTicket = (file: string): string => {
  for (const doc of parseLines(file)) {
    const [url] = artifactUrls(doc);
    if (url) return url;
  }
  return "";
};

const urandom = (size: number): Buffer => {
  const fd = fs.openSync("/dev/urandom", "r");
  try {
    const buf = Buffer.alloc(size);
    let offset = 0;
    while (offset < size) {
      offset += fs.readSync(fd, buf, offset, size - offset, null);
    }
    return buf;
  } finally {
    fs.closeSync(fd);
  }
};

const sameFile = (a: string, b: string): boolean => {
  try {
    return fs.readFileSync(a).equals(fs.readFileSync(b));
  } catch {
    return false;
  }
};

const steps = async (): Promise<void> => {
  log("1. start the creator (alice) on a private loopback mesh");
  start(
    ["create", "--nickname", "alice", "--state-file", `${RUN}/alice.state`],
    "alice",
  );
  if (!(await waitFor('"event":"ready"', `${RUN}/alice.log`, 60))) {
    fail("alice never became ready");
    printHead([`${RUN}/alice.log`, `${RUN}/alice.err`]);
    throw new Abort();
  }
  const gossip = gossipFrom(
    firstLine(readText(`${RUN}/alice.log`), '"event":"ready"'),
  );
  if (gossip === "") {
    fail("could not parse the gossip id");
    out(readText(`${RUN}/alice.log`));
    throw new Abort();
  }
  pass(`alice ready on ${gossip}`);

  log("2. join two more peers (bob, carol) as separate processes");
  for (const nick of ["bob", "carol"]) {
    start(
      ["join", gossip, "--nickname", nick, "--state-file", `${RUN}/${nick}.state`],
      nick,
    );
  }
  for (const nick of ["bob", "carol"]) {
    if (!(await waitFor('"event":"ready"', `${RUN}/${nick}.log`, 60))) {
      fail(`${nick} never became ready`);
      printHead([`${RUN}/${nick}.log`, `${RUN}/${nick}.err`]);
      throw new Abort();
    }
    pass(`${nick} ready`);
  }

  log("3. the roster: every peer sees the other two");
  await sleep(3000);
  for (const nick of NICKS) {
    const roster = exec(["peers", "--gossip", gossip, "--nickname", nick])
      .combined;
    const others = NICKS.filter(
      (other) => other !== nick && roster.includes(`"${other}"`),
    ).length;
    if (others === 2) pass(`${nick} sees both peers`);
    else fail(`${nick} sees ${others}/2 peers: ${roster.trimEnd()}`);
  }

  log("4. ping (RTT round-trip between processes)");
  // `ping` acks immediately and is fire-and-forget; the daemon emits `ping_report`
  // on its OWN stream ~10s later, so assert there rather than on this stdout.
  const pingAck = exec(["ping", "--gossip", gossip, "--nickname", "alice"])
    .combined;
  fs.writeFileSync(`${RUN}/ping.ack`, pingAck);
  if (await waitFor('"event":"ping_report"', `${RUN}/alice.log`, 30)) {
    const report = firstLine(
      readText(`${RUN}/alice.log`),
      '"event":"ping_report"',
    );
    pass(`ping_report: ${headBytes(report, 220)}`);
  } else {
    fail(
      `no ping_report on alice's stream within 30s (ack was: ${pingAck.trimEnd()})`,
    );
  }

  log("5. broadcast reaches every peer");
  exec([
    "a2a",
    "broadcast",
    "--gossip",
    gossip,
    "--nickname",
    "alice",
    "--text",
    "hello-everyone",
  ]);
  await sleep(3000);
  for (const nick of ["bob", "carol"]) {
    const polled = exec(["poll", "--gossip", gossip, "--nickname", nick]);
    if (polled.combined.includes("hello-everyone")) {
      pass(`${nick} received the broadcast`);
    } else {
      fail(`${nick} did not receive the broadcast`);
    }
  }

  log("6. directed frame reaches its addressee and NOT the bystander");
  exec([
    "a2a",
    "msg",
    "--gossip",
    gossip,
    "--nickname",
    "alice",
    "--to",
    "bob",
    "--text",
    "for-bob-only",
  ]);
  await sleep(5000);
  if (
    exec(["poll", "--gossip", gossip, "--nickname", "bob"]).combined.includes(
      "for-bob-only",
    )
  ) {
    pass("bob (the addressee) received the directed message");
  } else {
    fail("bob did not receive the directed message");
  }
  // The 0b9f438 property. Poll carol with a gap so an anti-entropy round
  // (which is what used to leak the frame) has fired in between.
  await sleep(8000);
  if (
    exec(["poll", "--gossip", gossip, "--nickname", "carol"]).combined.includes(
      "for-bob-only",
    )
  ) {
    fail(
      "carol (a bystander) saw a frame directed at bob — the confinement fix regressed",
    );
  } else {
    pass("carol never saw the frame directed at bob, including after anti-entropy");
  }

  log("7. blob offload: an artifact too large to inline");
  // `a2a artifact` is the *worker* returning a task result, so a task must exist
  // and its id is assigned by the worker and returned in the call response. The
  // file is far above the inline body ceiling, so it takes the blob ALPN.
  exec([
    "a2a",
    "broadcast",
    "--gossip",
    gossip,
    "--nickname",
    "bob",
    "--text",
    "warmup-back",
  ]);
  await sleep(2000);
  const payloadFile = `${RUN}/payload.bin`;
  fs.writeFileSync(payloadFile, urandom(3_000_000));

  // Task creation seals the request to bob's card, which propagates through the
  // `meta` doc asynchronously. Wait for it, or the request is dropped silently
  // and the call just hangs.
  for (let i = 0; i < 40; i++) {
    const meta = exec(["meta", "--gossip", gossip, "--nickname", "alice"]);
    if (meta.stdout.includes('"card"')) break;
    await sleep(500);
  }
  const call = exec([
    "a2a",
    "call",
    "--gossip",
    gossip,
    "--nickname",
    "alice",
    "--to",
    "bob",
    "--method",
    "SendMessage",
    "--text",
    "render the report",
    "--timeout-secs",
    "45",
  ]);
  fs.writeFileSync(`${RUN}/call.json`, call.combined);
  const taskId = findTaskId(`${RUN}/call.json`);

  if (taskId === "") {
    fail(`no task id from a2a call: ${headBytes(call.combined, 400)}`);
  } else {
    pass(`alice opened task ${taskId.slice(0, 8)}… on bob`);
    const artifact = exec([
      "a2a",
      "artifact",
      "--gossip",
      gossip,
      "--nickname",
      "bob",
      "--task-id",
      taskId,
      "--file",
      payloadFile,
      "--file-name",
      "payload.bin",
    ]);
    fs.writeFileSync(`${RUN}/artifact.json`, artifact.combined);

    if (artifact.status === 0) {
      pass("bob returned a 3 MB file result (offloaded over the blob channel)");
      await sleep(5000);
      // The initiator must surface it as a fetchable ticket, never as inline bytes.
      fs.writeFileSync(
        `${RUN}/alice.poll.json`,
        exec(["poll", "--gossip", gossip, "--nickname", "alice"]).stdout,
      );
      const ticket = findBlobTicket(`${RUN}/alice.poll.json`);
      if (ticket !== "") {
        pass(`alice surfaced a blob ticket, not inline bytes (${ticket.slice(0, 12)}…)`);
        const fetch = exec([
          "a2a",
          "fetch",
          ticket,
          "--output",
          `${RUN}/fetched.bin`,
        ]);
        fs.writeFileSync(`${RUN}/fetch.json`, fetch.combined);
        if (fetch.status === 0) {
          if (sameFile(payloadFile, `${RUN}/fetched.bin`)) {
            const size = fs.statSync(`${RUN}/fetched.bin`).size;
            pass(`a2a fetch pulled it back byte-identical (${size} bytes)`);
          } else {
            fail("fetched blob differs from the original");
          }
        } else {
          fail(`a2a fetch failed: ${headBytes(fetch.combined, 300)}`);
        }
      } else {
        fail("alice surfaced no blob ticket");
      }
    } else {
      fail(`artifact offload failed: ${headBytes(artifact.combined, 400)}`);
    }
  }

  log("8. clean shutdown");
  for (const nick of ["carol", "bob", "alice"]) {
    if (exec(["leave", gossip, "--nickname", nick]).status === 0) {
      pass(`${nick} left`);
    } else {
      fail(`${nick} failed to leave`);
    }
  }
};

const main = async (): Promise<void> => {
  try {
    await steps();
  } catch (error) {
    if (!(error instanceof Abort)) throw error;
    await cleanup();
    process.exitCode = 1;
    return;
  }
  if (failed) {
    // Keep the daemons and the run dir on failure, for inspection.
    out(`\n\x1b[31mE2E: failures above\x1b[0m — logs in ${RUN}\n`);
    process.exitCode = 1;
    return;
  }
  out("\n\x1b[32mE2E: all checks passed\x1b[0m\n");
  await cleanup();
};

await main();
