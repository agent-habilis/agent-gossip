// External agent 1 — the INITIATOR. A stock @a2a-js/sdk client pointed at
// daemon A's localhost JSON-RPC binding. It never speaks anything but
// standard A2A: card discovery, a broadcast SendMessage (the mesh-broadcast
// extension), directed task creation at /peers/<nick> (relayed over gossip),
// GetTask polling, and an approval follow-up into the task.
//
// Standalone usage: bun client.ts <state-file-of-daemon-A> <worker-nickname>
import {
  GetTaskRequest,
  SendMessageRequest,
  Task,
  taskStateToJSON,
} from "@a2a-js/sdk";
import {
  APPROVAL_TEXT,
  makeClientFactory,
  messageText,
  waitForSession,
} from "./common";

export interface InitiatorResult {
  selfCardName: string;
  taskId: string;
  createdState: string;
  parkedMetadata: Record<string, unknown> | undefined;
  completedState: string;
  completedText: string;
}

const textMessage = (text: string, taskId?: string) => ({
  message: {
    messageId: crypto.randomUUID(),
    role: "ROLE_USER",
    parts: [{ text }],
    ...(taskId ? { taskId } : {}),
  },
});

export async function runInitiator(
  stateFile: string,
  workerNick: string,
): Promise<InitiatorResult> {
  const session = await waitForSession(stateFile);
  const base = `http://127.0.0.1:${session.a2aPort}`;
  const factory = makeClientFactory(session.a2aToken);

  // 1. Discover our own daemon's card (unauthenticated well-known path) and
  //    broadcast a chat message to the whole square.
  const self = await factory.createFromUrl(base);
  const selfCard = await self.getAgentCard();
  await self.sendMessage(
    SendMessageRequest.fromJSON(textMessage("hello square — sent by @a2a-js/sdk")),
  );

  // 2. Discover the worker peer's card served by OUR daemon — it carries a
  //    relaying JSONRPC interface at /peers/<nick> — and create a task there.
  //    The worker's daemon mints the task id; the Task returns synchronously
  //    through the gossip request/response waiter.
  const worker = await factory.createFromUrl(
    base,
    `/peers/${workerNick}/.well-known/agent-card.json`,
  );
  const created = await worker.sendMessage(
    SendMessageRequest.fromJSON(
      textMessage("please answer: what is 6 * 7? return the result as an artifact"),
    ),
  );
  if (!("id" in created) || !created.status) {
    throw new Error(`expected a Task back, got: ${JSON.stringify(created)}`);
  }
  const task = created as Task;
  const createdState = taskStateToJSON(task.status.state);

  // 3. Poll GetTask until the worker's artifact parks the task in
  //    input-required for our approval. The served Task carries the state
  //    machine + mesh metadata (not history/artifacts — the artifact content
  //    rides the daemon's push plane; interop.test.ts asserts it off daemon
  //    A's --output json stream).
  const poll = async (accept: (current: Task) => boolean): Promise<Task> => {
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
      const current = await worker.getTask(
        GetTaskRequest.fromJSON({ id: task.id, historyLength: 50 }),
      );
      if (accept(current)) return current;
      await Bun.sleep(300);
    }
    throw new Error(`timed out polling task ${task.id}`);
  };

  const parked = await poll(
    (current) => taskStateToJSON(current.status!.state) === "TASK_STATE_INPUT_REQUIRED",
  );

  // 4. Approve — a follow-up SendMessage into the task. The worker then
  //    authors `completed` (native A2A server-completes semantics).
  await worker.sendMessage(
    SendMessageRequest.fromJSON(textMessage(APPROVAL_TEXT, task.id)),
  );

  const completed = await poll(
    (current) => taskStateToJSON(current.status!.state) === "TASK_STATE_COMPLETED",
  );

  return {
    selfCardName: selfCard.name,
    taskId: task.id,
    createdState,
    parkedMetadata: parked.metadata as Record<string, unknown> | undefined,
    completedState: taskStateToJSON(completed.status!.state),
    completedText: messageText(completed.status!.message),
  };
}

if (import.meta.main) {
  const [stateFile, workerNick] = Bun.argv.slice(2);
  if (!stateFile || !workerNick) {
    console.error("usage: bun client.ts <state-file-of-daemon-A> <worker-nickname>");
    process.exit(2);
  }
  try {
    const result = await runInitiator(stateFile, workerNick);
    console.log(`[client] connected to '${result.selfCardName}'`);
    console.log("[client] broadcast SendMessage accepted");
    console.log(`[client] task created: ${result.taskId} state=${result.createdState}`);
    console.log(
      `[client] artifact returned — task parked for review: ${JSON.stringify(result.parkedMetadata)}`,
    );
    console.log("[client] approval sent");
    console.log(`[client] task completed: ${result.completedText || "(no status text)"}`);
    console.log("[client] OK — full A2A task lifecycle through the mesh");
  } catch (error) {
    console.error(`[client] FAILED: ${error}`);
    process.exit(1);
  }
}
