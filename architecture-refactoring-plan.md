# Architecture Refactoring Plan

## Status

Deferred. The room join failure has higher priority. Do not begin this refactoring until it is explicitly resumed.

## Summary

Reshape the project around four conceptual layers while preserving CLI behavior, A2A ProtoJSON, the gossip wire format, state files, tickets, and public Rust paths during migration:

```text
Presentation
CLI · MCP · JSON output · human reports
                  ↓
Application
A2A services · commands · task policy · public API
                  ↓
Mesh runtime
membership · replication · routing · discovery · persistence
                  ↓
Protocol and transport
signed frames · mesh identity · crypto · iroh · multihop
```

Optimize first for contributor clarity: every entity has one owner, dependencies point downward, and folders reflect concepts rather than delivery mechanisms or historical growth.

## Target Architecture

### Workspace boundaries

Keep the existing crates:

- `agent-gossip`: composition root, A2A application, adapters, and public API.
- `agent-habilis-mesh`: payload-generic mesh runtime.
- `iroh-multihop-transport`: independent iroh transport.
- `slot-template`: pure templating.
- `tasks`: development tooling.
- `mesh-pipe`: architectural proof that the mesh engine is application-neutral.

Do not create an A2A crate while there is no second consumer. Create an internal dependency-pure `a2a::model` boundary that could be extracted mechanically later.

Prepare `agent-habilis-mesh::protocol` as a potential future `mesh-protocol` crate, but do not extract it until it no longer depends on runtime, lookup providers, filesystem behavior, logging, or broad utility modules.

### `agent-gossip` layout

```text
src/
├── a2a/
│   ├── model/          # Pure A2A v1.0 values and ProtoJSON validation
│   ├── extensions.rs   # agent-gossip A2A extension URIs and metadata keys
│   ├── binding/
│   │   ├── gossip/     # A2A ↔ generic mesh-frame mapping and validation
│   │   ├── jsonrpc/    # Transport-neutral A2A operation dispatch
│   │   ├── http/       # Localhost HTTP/SSE adapter
│   │   └── ipc/        # Unix-socket adapter
│   ├── service/
│   │   ├── messaging.rs
│   │   ├── tasks/
│   │   ├── cards.rs
│   │   └── blobs.rs
│   └── runtime/        # NodeDriver implementation and application timers
├── api/                # Stable in-process application facade
├── adapters/
│   ├── cli/
│   ├── mcp/
│   └── output/
├── tunnel/             # External A2A-server tunneling, tickets, expose/connect
└── harness/            # Feature-gated testing and benchmark access
```

Rules:

- `a2a::model` depends only on `std`, `serde`, `serde_json`, and `uuid`.
- Bindings translate external requests into application commands; they do not implement business policy.
- Task authorization, transitions, heartbeats, and artifact policy live in `a2a::service::tasks`.
- Rendering depends on application events; application services never depend on output rendering.
- HTTP, IPC, MCP, CLI, and the Rust API converge on the same application command vocabulary.
- Preserve `agent_gossip::a2a::*`, `agent_gossip::api::*`, and current CLI paths through facade re-exports.

### `agent-habilis-mesh` layout

```text
src/
├── protocol/
│   ├── identity/       # Mesh, participant, message, and correlation identities
│   ├── frame/          # Frame kinds, encoding, canonical bytes, signatures
│   ├── crypto/         # Derivation, passwords, and sealing
│   ├── shard/          # Pure splitting and shard validation
│   └── ticket/         # Shared engine-level ticket primitives
├── runtime/
│   ├── node.rs
│   ├── driver.rs
│   ├── event_loop/
│   ├── state/
│   ├── setup/
│   └── shutdown.rs
├── membership/         # Presence, heartbeat, roster, and reach
├── replication/
│   ├── history/
│   ├── antientropy/
│   ├── reassembly/
│   └── documents/
├── routing/
│   ├── gossip/
│   ├── unicast/
│   └── multihop.rs
├── discovery/
│   ├── rendezvous/
│   ├── lookup/
│   ├── directory/
│   └── invite/
├── blob/
├── persistence/        # State file and durable local session metadata
├── observability/      # Logging and process diagnostics
└── config/             # Runtime tuning and configuration
```

Keep the runtime as a single owning actor. Improve its internal structure instead of distributing mutable mesh state across actors or locks.

Partition `EventLoopState` into cohesive owned entities:

```text
EventLoopState
├── MembershipState
├── HistoryState
├── ReplicationState
├── DocumentState
├── RequestState
├── RoutingState
└── SecurityState
```

Handlers should receive only the relevant substate and capabilities instead of unrestricted access to the complete state object.

Replace application-specific growth in `NodeDriver` with a smaller generic contract based on:

- Application command handling
- Validated application-frame handling
- Lifecycle hooks
- A single optional application deadline
- Application events emitted back to the runtime

Keep HTTP, IPC, polling, and surfaced-event rings in `agent-gossip`; the generic engine must not name those delivery mechanisms.

## Entity and Communication Corrections

Use distinct names consistently:

| Layer | Entity |
| --- | --- |
| Transport | endpoint, link, unicast connection, route |
| Mesh identity | mesh, mesh ID, mesh name |
| Cryptographic identity | participant identity |
| Membership | participant, roster entry, reach |
| Protocol | frame, frame ID, frame kind |
| A2A | message, task, artifact, agent card |
| Presentation | output event, JSON line, report |

Rename the engine's wire-level `Message` to `Frame` behind a compatibility alias. This removes ambiguity with A2A `Message`.

Standardize outbound command flow:

```text
CLI / MCP / HTTP / IPC / Rust API
                 ↓
        ApplicationCommand
                 ↓
          A2A services
                 ↓
     MeshCommand / generic AppFrame
                 ↓
          Mesh runtime
                 ↓
       Gossip or unicast route
```

Standardize inbound flow:

```text
Gossip or unicast bytes
          ↓
decode → size/signature/history/dedup validation
          ↓
infrastructure frame ──→ mesh subsystem
application frame ─────→ application binding validation
                          ↓
                    A2A service
                          ↓
                    DomainEvent
                          ↓
              API / poll / output adapters
```

Define separate event types:

- `MeshEvent`: validated infrastructure and generic application-frame events.
- `ApplicationEvent`: A2A messages, task changes, roster projections, and operation results.
- `OutputEvent`: compatibility-oriented presentation projection.

Do not let `OutputEvent` become the domain event bus.

Replace broad `util` ownership:

- Bounded collections move to a small internal `collections` module.
- Tuning moves to `config`.
- Logging and memory diagnostics move to `observability`.
- Process handling moves to runtime or adapters.
- Output helpers move to presentation or development tooling.
- Protocol constants move beside the protocol types they constrain.

## Migration Sequence

1. **Protect contracts**
   - Expand snapshots for A2A responses, cards, and mesh frames.
   - Record public Rust re-exports, JSON output, IPC, MCP, tickets, and state-file compatibility.
   - Require structural refactors to leave snapshot content unchanged.

2. **Create the pure A2A model**
   - Move A2A values and validation into `a2a::model`.
   - Move mesh extension constants and frame tags outside the model.
   - Retain existing public paths through re-exports.

3. **Separate application services from adapters**
   - Introduce one application-command vocabulary used by API, CLI, MCP, HTTP, and IPC.
   - Move task policy and card construction into services.
   - Restrict adapters to parsing, authentication, command conversion, and rendering.

4. **Correct event ownership**
   - Introduce `ApplicationEvent` between services and presentation.
   - Project it into the existing `OutputEvent` without changing output.
   - Remove output dependencies from task and messaging services.

5. **Partition mesh runtime state**
   - Split `EventLoopState` into owned subsystem states while retaining one actor.
   - Narrow helper and handler parameters.
   - Group the central `select!` arms into ingress, maintenance, bootstrap, application, and shutdown components.

6. **Reorganize mesh subsystems**
   - Consolidate gossip, unicast, and multihop under routing.
   - Consolidate logs, anti-entropy, reassembly, and CRDT documents under replication.
   - Consolidate rendezvous, lookup, directories, and invites under discovery.
   - Move state-file behavior into persistence.

7. **Narrow the engine facade**
   - Expose configuration, protocol values, node lifecycle, commands, and events through curated modules.
   - Make runtime internals crate-private.
   - Remove application access to engine `util` and internal daemon structures.

8. **Prepare the protocol boundary**
   - Ensure protocol modules contain pure encoding, validation, identity, crypto, and sharding.
   - Separate encoded mesh descriptors from resolved iroh lookup configuration.
   - Reassess a `mesh-protocol` crate only when the boundary has a low dependency floor or another runtime consumer exists.

9. **Clean compatibility shims**
   - Remove aliases and legacy module paths only in a deliberate breaking release.
   - Until then, prefer deprecated re-exports over broad downstream churn.

## Test Plan and Acceptance Criteria

- Run all long tests in the background as required by the repository.
- For each migration step, run focused unit tests, snapshot tests, `cargo task lint`, then `cargo task test`.
- Add dependency-boundary checks ensuring `a2a::model` cannot import the mesh engine or runtime libraries.
- Maintain byte-identical A2A ProtoJSON, mesh frames, JSON output, tickets, state files, and IPC/MCP responses.
- Preserve the `mesh-pipe` build and tests after every engine-boundary change.
- Add architectural tests or compile-only fixtures proving:
  - A minimal non-A2A application can implement the mesh driver.
  - A2A model types compile without Tokio or iroh.
  - Gossip and unicast feed the same validated-frame ingestion path.
  - Presentation can be removed without affecting task or mesh services.

Completion means a contributor can identify an entity's owner from its name and folder, application policy does not leak into the mesh engine, delivery adapters do not own domain behavior, and potential crate boundaries are clean without creating crates solely for organization.

## Assumptions

- This is the ideal target architecture, implemented incrementally rather than as one large move.
- Contributor clarity takes priority over minimizing file movement.
- Wire, CLI, JSON, MCP, IPC, tickets, state files, and current public Rust paths remain compatible throughout migration.
- There is no second A2A consumer, so `a2a::model` remains an internal module.
- The mesh runtime retains single-actor state ownership to preserve ordering and avoid new synchronization complexity.
- Crate extraction occurs only when it creates dependency isolation or supports a real consumer.
