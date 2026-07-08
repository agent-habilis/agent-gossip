// Rerunnable harness for the interop scenario: two external @a2a-js/sdk
// agents completing a full A2A task lifecycle through two agent-square
// daemons bridged by --a2a-serve. Deterministic — no LLM anywhere, every
// exchanged string is a literal (see client.ts/worker.ts) — so this is safe
// to run repeatedly (`bun test`), including back-to-back in the same shell:
// startMesh() mints a fresh work dir and OS-assigned ports every call.
import { afterAll, beforeAll, expect, test } from "bun:test";
import { runInitiator } from "./client";
import { type Mesh, startMesh } from "./common";
import { runWorker } from "./worker";

let mesh: Mesh;

beforeAll(async () => {
  mesh = await startMesh();
}, 30_000);

afterAll(() => {
  mesh.stop();
});

test(
  "two external @a2a-js/sdk agents complete a task through the agent-square bridge",
  async () => {
    const [initiator, worker] = await Promise.all([
      runInitiator(mesh.stateA, mesh.sessionB.nickname),
      runWorker(mesh.stateB),
    ]);

    expect(initiator.selfCardName).toBe(mesh.sessionA.nickname);
    expect(initiator.taskId).toBe(worker.taskId);
    expect(initiator.createdState).toBe("TASK_STATE_SUBMITTED");
    expect(worker.pickedUpMetadata?.["mesh:peer"]).toBe(mesh.sessionA.nickname);
    expect(initiator.parkedMetadata?.["mesh:review"]).toBe(true);
    expect(worker.approvedBall).toBe("worker");
    expect(initiator.completedState).toBe("TASK_STATE_COMPLETED");

    // The artifact content rides the daemon's push plane, not GetTask —
    // assert it surfaced on the initiator daemon's --output json stream.
    const daemonALog = await Bun.file(mesh.daemonALogPath).text();
    expect(daemonALog).toContain("the answer is 42");
  },
  65_000,
);
