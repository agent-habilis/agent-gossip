// External agent 2 — the WORKER, attached to daemon B. It discovers incoming
// tasks with the stock @a2a-js/sdk client (ListTasks/GetTask against daemon
// B's localhost binding). The worker legs — status updates and the artifact —
// are not JSON-RPC methods on the binding (the daemon is the A2A server; the
// agent behind it authors those frames), so they go through the
// `agent-gossip a2a status|artifact` CLI against the same daemon.
//
// Standalone usage: bun worker.ts <state-file-of-daemon-B>
import {
  GetTaskRequest,
  ListTasksRequest,
  Task,
  taskStateToJSON,
} from "@a2a-js/sdk";
import { makeClientFactory, runAgentGossip, waitForSession } from "./common";

export interface WorkerResult {
  taskId: string;
  pickedUpMetadata: Record<string, unknown> | undefined;
  approvedBall: string | undefined;
}

export async function runWorker(stateFile: string): Promise<WorkerResult> {
  const session = await waitForSession(stateFile);
  const base = `http://127.0.0.1:${session.a2aPort}`;
  const client = await makeClientFactory(session.a2aToken).createFromUrl(base);

  const workerLeg = (taskId: string, args: string[]) =>
    runAgentGossip([
      "a2a",
      ...args.slice(0, 1),
      "--gossip",
      session.gossip,
      "--nickname",
      session.nickname,
      "--task-id",
      taskId,
      ...args.slice(1),
    ]);

  // 1. Poll ListTasks until a fresh task lands (created by the initiator's
  //    directed SendMessage, relayed to this daemon over gossip).
  let pending: Task | undefined;
  const discoveryDeadline = Date.now() + 60_000;
  while (!pending && Date.now() < discoveryDeadline) {
    const listed = await client.listTasks(ListTasksRequest.fromJSON({}));
    pending = listed.tasks.find((task) => {
      const state = taskStateToJSON(task.status?.state ?? 0);
      return state === "TASK_STATE_SUBMITTED" || state === "TASK_STATE_WORKING";
    });
    if (!pending) await Bun.sleep(300);
  }
  if (!pending) throw new Error("no task arrived within 60s");
  const pickedUpMetadata = pending.metadata as Record<string, unknown> | undefined;

  // 2. Accept, answer, and park the task for approval. The "skill" is
  //    deliberately trivial: the demo prompt asks for 6 * 7. (The request
  //    text itself rides the daemon's event stream / `agent-gossip poll`,
  //    not the served Task — GetTask carries the state machine + mesh
  //    metadata.)
  await workerLeg(pending.id, ["status", "--state", "working", "--text", "computing"]);
  await workerLeg(pending.id, ["artifact", "--text", "the answer is 42"]);

  // 3. The artifact hands the ball to the initiator; their approval
  //    follow-up hands it back. Poll the served mesh:ball metadata for that
  //    flip, then author completed — the worker owns task completion in A2A.
  let approvedBall: string | undefined;
  const approvalDeadline = Date.now() + 60_000;
  while (!approvedBall && Date.now() < approvalDeadline) {
    const current = await client.getTask(
      GetTaskRequest.fromJSON({ id: pending.id, historyLength: 50 }),
    );
    const ball = current.metadata?.["mesh:ball"];
    if (ball === "worker") approvedBall = ball;
    else await Bun.sleep(300);
  }
  if (!approvedBall) throw new Error("initiator never approved within 60s");

  await workerLeg(pending.id, ["status", "--state", "completed", "--text", "done"]);

  return { taskId: pending.id, pickedUpMetadata, approvedBall };
}

if (import.meta.main) {
  const [stateFile] = Bun.argv.slice(2);
  if (!stateFile) {
    console.error("usage: bun worker.ts <state-file-of-daemon-B>");
    process.exit(2);
  }
  try {
    const result = await runWorker(stateFile);
    console.log(
      `[worker] picked up task ${result.taskId}: ${JSON.stringify(result.pickedUpMetadata)}`,
    );
    console.log("[worker] artifact returned, waiting for approval");
    console.log("[worker] task completed — OK");
  } catch (error) {
    console.error(`[worker] FAILED: ${error}`);
    process.exit(1);
  }
}
