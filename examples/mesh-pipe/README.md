# mesh-pipe

A Unix pipe over the gossip mesh — the **second consumer** of the
[`agent-habilis-mesh`](../../agent-habilis-mesh) engine, and a deliberately
**non-a2a** one. It exists to prove the engine is *payload-generic*: the same
mesh, discovery, and directed-routing stack that carries
`agent-gossip`'s A2A tasks will carry **raw bytes** just as happily, because the
engine never inspects an `App` frame's payload — it only routes on the frame's
tag/addressee.

## What it demonstrates

The engine's application seam is two traits — `NodeApp` (classify + dispatch
inbound `App` frames) and `NodeDriver` (lifecycle + typed inputs). `agent-gossip`
is one implementor (its A2A data model). `mesh-pipe` is a completely
independent second one:

- **Its own `AppTag` taxonomy.** Two tags in the pipe's own namespace —
  `pipe_data` (one ordered slice of the byte stream) and `pipe_eof` (the source
  ended). The a2a layer's `a2a_msg` / `a2a_status` / … never appear.
- **~40 lines of app logic.** `classify` marks pipe frames ephemeral
  (`loggable: false`, not a beat, always valid, unchained); `on_app_frame`
  base64-decodes a `pipe_data` body to stdout and exits on `pipe_eof`. Every
  other `NodeApp`/`NodeDriver` hook uses the engine's **default** (no-op / `None`)
  bodies — a minimal receive-only consumer implements only `classify` +
  `on_app_frame` and sets the three associated types to trivial types.
- **The generic outbound primitive.** `listen` emits frames through
  `agent_habilis_mesh::gossip::send_app(state, ctx, tag, to, corr, body)` —
  the engine's payload-agnostic build → sign → route helper.
- **The generic in-process node.** It runs the event loop in-process via
  `agent_habilis_mesh::daemon::Node<PipeApp>` — the app-agnostic handle that
  `agent-gossip`'s `api::MeshSession` now wraps rather than duplicates.

It depends on **only** the engine crate (plus `tokio` / `anyhow` / `clap` /
`base64`) — never on `agent-gossip` or its a2a layer.

> Bytes ride base64-encoded in the frame body because `MessageBody` is
> text/JSON-shaped (it rejects control bytes). `send_app` sends a single
> (unsharded) frame, so `listen` chunks stdin below one frame's worth of raw
> bytes.

## Run it

Two terminals, a shared `--topic` string as the rendezvous:

```sh
# terminal 1 — send
echo 'hello over the gossip mesh' | mesh-pipe listen --topic demo

# terminal 2 — receive (prints "hello over the gossip mesh")
mesh-pipe connect --topic demo
```

`--to <nick>` on `listen` sends **directed** frames to one peer instead of
broadcasting to every `connect`. You can also rendezvous by id: bare
`mesh-pipe listen` mints a loopback mesh and prints its `💬…` id on stderr;
pass that id to `mesh-pipe connect 💬…`. Discovery flags mirror the CLI:
`--public` / `--mdns` / `--dht` / `--relay` on `listen` create a reachable
mesh.

### Sizing

- `--max-peers <N>` — cap on active-view neighbours (default: the engine's full
  active-view capacity). A small cap forces a **partial mesh**, so a peer beyond
  the active view is no longer a gossip neighbour.

There is no transport policy to steer. A **directed** (`--to`) frame goes over
the point-to-point **unicast** channel — that is the only directed transport.
Reaching a peer with no direct path is the **multihop** transport's job, and
multihop is a real iroh custom transport rather than a send tier, so nothing
chooses between them: iroh's path selection does. mesh-pipe leaves multihop off
(`multihop: false`), so on a loopback mesh every addressee is directly dialable
and directed frames take the direct path.

> **Directed (`--to`) delivery works for plaintext frames.** Whether a directed
> `App` frame is *sealed* (encrypted) to its addressee is **app-controlled** per
> frame (`AppClass.sealed`, set by `classify`). a2a marks its directed tags
> sealed, so the engine's inbound gate still unseals-or-drops them (no security
> change). mesh-pipe marks `pipe_data`/`pipe_eof` **`sealed: false`**, so the
> addressee passes the plaintext (base64, unsealed) body straight through to
> `on_app_frame` — no card, no X25519 key, no seal required. The frame is still
> signature-authenticated before the gate; `sealed: false` only means "not
> additionally encrypted". So a directed `pipe_data` frame is delivered
> end-to-end over the unicast channel.

## Test it

```sh
cargo test -p mesh-pipe
```

`tests/roundtrip.rs` spawns a `listen` and a `connect` on a loopback mesh, pipes
a known payload into the sender, and asserts it comes out of the receiver
byte-for-byte — proving plaintext delivery works end-to-end for a non-a2a
consumer.

Multi-hop delivery is **not** covered here: mesh-pipe runs with multihop off, so
it has no multi-hop path to exercise. That coverage lives in
[`iroh-multihop-transport`](../../crates/iroh-multihop-transport)'s own
`tests/e2e.rs`, which builds nodes whose *only* transport is multihop and then
asserts the connection's selected path really was multihop — the one assertion
that makes such a test mean anything.

## What it is not

This is a *versatility* demo, not a bandwidth tool. Every chunk is a signed,
logged, re-broadcast gossip frame — gossip is the wrong plane for bulk transfer.
It shines for small interactive streams and multi-consumer broadcast (every
`connect` peer gets a copy for free), and it intentionally drops the ticket /
ALPN / follow / password / throttle / bench machinery a dedicated
direct-QUIC pipe would carry.
