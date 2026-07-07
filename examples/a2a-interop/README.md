# a2a-interop

Two **external** A2A agents — built on the official
[`@a2a-js/sdk`](https://github.com/a2aproject/a2a-js) — delegating and
completing a task **through the mesh**, over the localhost JSON-RPC binding
(`--a2a-serve`). Where [`mesh-pipe`](../mesh-pipe) proves the engine is
payload-generic, this example proves the A2A surface is
**implementation-generic**: software we didn't write, speaking stock A2A
v1.0, interoperates with agent-square's two daemons end to end.

```
client.ts (initiator) ──HTTP/JSON-RPC──▶ daemon A ═══ gossip ═══ daemon B ◀──HTTP/JSON-RPC── worker.ts (worker)
        @a2a-js/sdk                                                                              @a2a-js/sdk
```

## Why `@a2a-js/sdk@1.0.0-beta.0`

agent-square speaks **A2A spec v1.0** — ProtoJSON encoding,
`SCREAMING_SNAKE` enums, PascalCase JSON-RPC methods (`SendMessage`,
`GetTask`). The SDK's stable `0.3.x` line still speaks the older v0.3 wire
(`message/send`, inline `kind` tags) and cannot interoperate; the `next`
dist-tag (`1.0.0-beta.0`) is the spec-1.0 line. Pin accordingly.

## What each step exercises

| step | A2A surface |
|---|---|
| card discovery | `/.well-known/agent-card.json` (own) and `/peers/<nick>/.well-known/agent-card.json` (a member's, carrying the relaying `JSONRPC` interface) |
| auth | `Authorization: Bearer <token>` from the daemon's `--state-file` (`a2a_port` / `a2a_token`) |
| broadcast chat | `SendMessage` with no addressee (the `mesh-broadcast` extension) |
| task creation | directed `SendMessage` at `/peers/<nick>` — relayed over the gossip request/response plane; the worker's daemon mints the task id and the `Task` returns synchronously |
| task discovery (worker side) | `ListTasks` / `GetTask` against the worker's own daemon |
| worker legs | `agent-square a2a status\|artifact` — the daemon is the A2A server; the agent *behind* it authors status/artifact frames, so these are CLI, not client JSON-RPC methods |
| review round-trip | artifact parks the task `input-required` and hands the ball to the initiator (`metadata["mesh:ball"]`); the approval follow-up (`SendMessage` with `taskId`) hands it back; the worker authors `completed` |

The served `Task` object carries the state machine and the mesh metadata,
**not** `history`/`artifacts` — the artifact content rides the daemon's push
plane (the `--output json` stream / `agent-square poll`), which is why
`interop.test.ts` asserts the artifact text off daemon A's event stream and
the scripts poll states + `mesh:ball` rather than task history.

## Run it

Needs [bun](https://bun.sh) and a built `agent-square` binary
(`cargo build`; `AGENT_SQUARE_BIN` overrides the
`target/debug/agent-square` default).

```sh
bun install
bun test
```

`bun test` starts both daemons on a fresh loopback square (`startMesh()` in
`common.ts`), runs one external agent against each concurrently, and asserts
on the whole lifecycle — `submitted → working → input-required →
completed`, plus the artifact content arriving on the initiator's stream.
Everything here is deterministic (no LLM; every exchanged string is a
literal), and `startMesh()` mints a fresh temp dir + OS-assigned ports every
call, so the test is safe to re-run any number of times back to back.

The two agents also run standalone against daemons you started yourself:

```sh
bun client.ts <state-file-of-daemon-A> <worker-nickname>
bun worker.ts <state-file-of-daemon-B>
```
