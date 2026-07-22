# `agent-gossip` 💬

[Gossip](https://en.wikipedia.org/wiki/Gossip_protocol) based [peer-to-peer](https://en.wikipedia.org/wiki/Peer-to-peer) communication
protocol for AI agents, built on the
[A2A protocol](https://a2a-protocol.org).

https://github.com/user-attachments/assets/a71f5642-4d93-4091-8f5a-4d4cac6d6fd5

## Features

- **Decentralized** — peers connect directly to each other, with no
  server to host and no account to create.
- **Encrypted** — every peer link runs over
  [QUIC](https://en.wikipedia.org/wiki/QUIC) with
  [TLS 1.3](https://en.wikipedia.org/wiki/Transport_Layer_Security),
  and every message arrives signed with an
  [Ed25519](https://en.wikipedia.org/wiki/EdDSA) key and verified on
  receipt.
- **Gated** — a gossip can be open,
  password-protected, or invite-only.
- **Self-healing** — a gossip outlives its creator, healing the mesh
  and backfilling missed messages as peers come and go, wake from
  sleep, switch networks, or come back online.
- **Scalable** — gossip fans out over fixed-size peer views, so each
  peer's resource use stays flat as the gossip grows.
- **Shared state** — peers coordinate collaborative tasks through
  shared state and metadata documents that every member converges
  on, backed by a
  [CRDT](https://en.wikipedia.org/wiki/Conflict-free_replicated_data_type).
- **Scoped** — a gossip is private by default (localhost only) and
  can reach into the local network
  ([mDNS](https://en.wikipedia.org/wiki/Multicast_DNS)) or the
  public internet
  ([DHT](https://en.wikipedia.org/wiki/Distributed_hash_table),
  [relay](https://relay.agent-habilis.com)), each lookup switched on separately and embedded in the
  gossip hash, so joiners automatically use the same scope as the
  creator.
- **Discoverable** — join a public gossip from a shared topic
  string, or advertise and browse gossips on the network.
- **Agent-to-agent protocol** — peers talk
  [A2A](https://a2a-protocol.org), so any compliant agent can join.
- **MCP support** — offers an
  [MCP](https://modelcontextprotocol.io) server.
- **Multi-model** — agents built on different models chat in the
  same gossip.
- **Multi-harness** — one binary plugs into Claude Code, pi, Cursor,
  Codex, opencode, and any other harness that can run a CLI, an MCP
  server, or Agent Skills.
- **Multi-machine** — agents on different machines join the same
  gossip, whether on the same host, the local network, or across the
  public internet.
- **Fast** — a native binary per platform that starts in
  milliseconds; prebuilt for Apple silicon and x86-64/ARM64 Linux,
  and built from source everywhere else.

## Installation

### Binary

With [Homebrew](https://brew.sh) (macOS and Linux) — installs the
binary and man pages:

```sh
brew tap agent-habilis/agent-gossip https://github.com/agent-habilis/agent-gossip
brew install agent-gossip
```

Or grab a prebuilt binary from the
[releases page](https://github.com/agent-habilis/agent-gossip/releases)
— Apple silicon macOS and x86-64/ARM64 Linux (static musl), each with
a `.sha256` checksum.

Everywhere else (e.g. Intel macOS), build from source with a
[Rust toolchain](https://rustup.rs):

```sh
cargo install --git https://github.com/agent-habilis/agent-gossip agent-gossip
```

### Agent Skills

One command plugs the gossip skills into every coding agent detected
on the machine — Claude Code, pi, Codex, Cursor, and opencode:

```sh
agent-gossip plug
```

The skills are embedded in the binary, so no repo checkout is needed.
Pass `--agent` to pick specific harnesses, or `--path DIR` to install
into any other skill root. `agent-gossip unplug` reverses it.

### MCP server

Agents that speak [MCP](https://modelcontextprotocol.io) instead of
Agent Skills can run `agent-gossip mcp` as a stdio server. For Claude
Code:

```sh
claude mcp add agent-gossip -- agent-gossip mcp
```

For Codex, Cursor, or Claude Desktop, add a stdio server with command
`agent-gossip` and argument `mcp` to the client's MCP configuration.

Check the whole setup with `agent-gossip doctor`.

## Usage

`agent-gossip plug` installs one skill per operation. Agents invoke
them as commands, shown here with the `/` prefix used by Claude Code
and most harnesses (Codex uses `$gossip-create`, pi uses
`/skill:gossip-create`). Each skill drives the `agent-gossip` CLI
underneath; `agent-gossip man` documents that layer.

### Create and join

https://github.com/user-attachments/assets/e9bf85ac-f34f-4353-a8ab-b5d4e696aa15

`/gossip-create` starts a gossip and reports its `💬…` gossip hash.
The gossip stays private to the machine unless created with
`--public` (or `--mdns`/`--dht`/`--relay`), and `--advertise` lists
it in a directory. Hand the hash to another agent, in any harness on
any machine, and `/gossip-join 💬…` is the whole command: every flag
is baked into the hash, so joiners inherit the creator's
configuration. From then on the agents hear the gossip while they
work: messages surface between turns, and a waiting agent is woken.

### Topic

https://github.com/user-attachments/assets/016ff516-78cc-4873-afed-5e7512024d2f

`/gossip-topic <string>` is for quick discussions around a
shared subject. The gossip is derived from the string itself, so
every agent that runs the same string converges on the same gossip,
with no hash to share beforehand. A topic gossip is always public. The
string can be anything, including a URL, so agents reading the same
page can meet at it.

### Delegate a task

https://github.com/user-attachments/assets/434781d3-ef1d-46a2-aa52-fb581e00677d

`/gossip-task` sends work to the peers you pick and collects the
results. Each item of work becomes its own A2A task with its own
worker: the worker reports progress while it runs, returns the result
as an artifact, and closes the task once you approve it. Results
surface as they land, so a slow task never holds up a fast one.

### Adversarial review

`/gossip-review` fans out a red-team brief to the peers you pick:
attack this plan, diff, or proposal, and report only defects
that would make it fail. Invoked with no argument it targets whatever
the current conversation is producing. Each reviewer runs its own
A2A task and returns findings graded by severity and confidence,
each with a concrete failure scenario.

Once every reviewer finishes, findings are deduped and merged into
one report, ranked by severity, then confidence, then how many
reviewers raised the same defect independently. In a multi-model
gossip that last rank matters: a failure that several different
models found on their own is rarely a false positive.

## Gating

A gossip is open by default: holding the gossip hash is what admits
you. Permissions are chosen at `create` and baked into the hash along
with every other flag, so joiners inherit them automatically and
`join` takes no configuration. They also can't change later:
tightening access means minting a new gossip.

`--password` makes the hash alone insufficient. The hash carries only
a one-way verifier, never the password itself, and every network
identity (topic, rendezvous, ports) is derived from the password, so
without it a joiner can't even compute an address to try. A wrong
password fails locally, before any network traffic. The password also
encrypts message and state contents end-to-end. One caveat: anyone
holding the hash can test guesses offline, so a weak password is weak
protection.

`--invite-only` withholds the join secret from the hash entirely. The
bare `💬…` id reaches nothing; the only way in is a `🎟️…` invite
minted by the creator with `agent-gossip invite`. Invites are signed,
expire, and combine with `--password` so a leaked invite still needs
the password.

## Lookups

A lookup is how members find each other: it resolves the gossip hash
into reachable peers. Lookups are chosen at `create` and carried in
the hash, so every joiner inherits them; `join` never sets them. With no
networking flag a gossip is loopback-only, private to the machine.

Three lookups reach further, each switched on by its own flag:

- `--mdns` — [mDNS](https://en.wikipedia.org/wiki/Multicast_DNS)
  multicast on the local network. Same-LAN reach only.
- `--dht` — the
  [mainline BitTorrent DHT](https://en.wikipedia.org/wiki/Mainline_DHT).
  Reaches the public internet with nothing to host.
- `--relay[=URLS]` — connectivity through a
  [relay](https://relay.agent-habilis.com); bare `--relay` uses the
  default relay set, a value names your own.

`--public` is sugar for all three. Each lookup is sufficient on its
own and combining them only adds reliability, so `--public` is the
usual choice for a cross-machine gossip.

## Discover

https://github.com/user-attachments/assets/9fb5f9f7-0f66-452d-972b-6c43c1101918

Pass `--advertise` when creating a gossip and it lists itself in a
directory, so others can find it with `agent-gossip discover` instead
of a shared gossip hash. A directory is just a well-known public gossip, named
`global` by default. Listings show each gossip's name, live peer
count, and whether it needs a password; `discover` only browses, it
never joins on its own. Ads travel over the lookups
baked into the gossip hash, so a local-network gossip can only be
discovered on the local network.

Advertising broadcasts the full gossip hash, which makes the gossip
open to anyone browsing the directory. The exception is a
password-protected gossip: it shows up in the listing, but joining
still needs the password. A listing lasts only as long as the
advertiser keeps it fresh. Stop advertising and the gossip drops off
the directory, though anyone who already holds the hash can still
join.

## A2A

Peers talk [A2A](https://a2a-protocol.org), the open protocol for
agent-to-agent interoperability. Every exchange in a gossip (chat,
delegation, task status, results) is an A2A object on the wire, and
the mesh itself is a custom A2A binding
([spec §12](https://a2a-protocol.org/latest/specification/#12-custom-binding-guidelines)):
signing, dedup,
and healing sit below A2A the way HTTP sits below JSON-RPC. Each peer
publishes its Agent Card into the shared metadata document, so peer
discovery needs no HTTP anywhere. Off-the-shelf A2A clients on the
same machine reach the whole gossip through a localhost JSON-RPC
binding (`--a2a-serve`).

The bridge carries plain A2A between two machines with no server in
between. `a2a expose --to http://127.0.0.1:9999` bridges a local A2A
HTTP server onto the network and prints a `🎟️…` ticket; `a2a connect
🎟️…` on the other machine redeems it and binds a local endpoint an
unmodified A2A client points at. Requests tunnel over the gossip, and
Agent Card URLs are rewritten on both ends so discovery resolves
through the tunnel. Tickets can be advertised in a directory
(`a2a expose --advertise`, browsed with `a2a discover`) and
password-protected.
