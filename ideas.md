# Feature ideas

A research catalog for future `agent-gossip` features, in the spirit of `forum`:
small, composable, obviously useful, fun. Sources: IRC/mIRC/IRCv3/XMPP/
Matrix/Discord; the 2025–26 agent-interop landscape (A2A, MCP, FIPA, contract
net, blackboard, consensus); the P2P tooling ecosystem (magic-wormhole, croc,
Syncthing, iroh, Holepunch/Keet, cabal, SSB, nostr, Tailscale,
Willow/Earthstar).

Two findings worth keeping in mind:

- **Convergence:** message tags/filtering, capability cards, and rich
  busy/idle status surfaced independently in all three research streams.
- **Self-selection beats assignment:** agent-gossip delegates by *naming* a worker;
  both classic MAS results (contract net, blackboard) and 2025 LLM research
  say letting workers claim or bid on tasks outperforms master–slave
  dispatch.

## Shortlist (user-selected)

- [ ] **notice** — in progress
- [ ] tag
- [ ] status
- [ ] board
- [ ] clip

---

## Tier 1 — forum-shaped: tiny surface, rides existing primitives

### notice — no-auto-reply message class *(in progress)*
Origin: IRC `NOTICE` (RFC 1459). IRC's rule — never auto-respond to a
NOTICE — is what kept bots from reply-looping each other, and LLM agents
reflexively answer everything, so the bit matters more here. A distinct
message kind with a documented contract makes CI results, status broadcasts,
and log lines safe by construction.
`agent-gossip notice --swarm 💬… --text "build green"` → `"type":"notice"`.

### tag — message tags + filtered poll
Origin: MQTT/NATS subjects, cabal channels, IRC channels. One swarm is one
firehose; every agent parses everything and burns context. A free-string tag
on `msg`/`notice` plus daemon-side filtering on `poll` (and MCP
`fetch_messages`) gives sub-channels (`ci`, `reviews`, `alerts`) with zero
new infrastructure. One tag per message; `--tag a,b` filters as OR;
non-message events (presence, state, task) always pass; untagged messages
match only an unfiltered poll. (Named `tag`, not `topic` — the gossip layer
owns `topic`.)
`agent-gossip msg … --tag ci` / `agent-gossip poll … --tag ci,alerts`.

### status — busy/idle presence with a note
Origin: ICQ away/DND, IRCv3 away-notify, A2A task states. Presence answers
"alive?"; delegation needs "available?". Design: meta-channel convention
`/peers/<nick>/status = {state: busy|idle, note, ts}` (zero wire changes;
the meta log backfills whole for late joiners), surfaced only through the
live roster so departed peers' stale statuses never render.
`agent-gossip status busy --note "running tests, ~4m"` / bare `agent-gossip status` prints
the roster with statuses.

### board — blackboard task board with self-selection
Origin: blackboard architectures (Hearsay-II); arXiv 2510.01285 reports
13–57 % improvement over master–slave assignment. Post tasks to a shared
board; idle agents claim them. Pure convention over the `state` channel
under `/board/<id> = {desc, status: open|claimed|done, by, opened_by, ts}`;
claim races resolve deterministically by the existing log total order
(last-writer-wins; `claim` re-reads and reports "claimed by you" / "lost
to <nick>"). Ids: short base58 (4 chars), re-minted on collision.
`agent-gossip board post "write lookup tests"` / `board claim b7Kq` / `board done b7Kq` / `board ls`.

### vote — ballots with a deterministic tally
Origin: Discord polls, Matrix MSC3381, multi-agent consensus literature.
"Three agents disagree" today resolves by prose-parsing. A ballot is a typed
object in the state channel (question, options, deadline, rule); votes are
signed merges; the tally is deterministic for every member for free, thanks
to log convergence.
`agent-gossip vote open "merge strategy?" --option squash --option rebase --deadline 2m` / `vote cast <id> rebase`.

### card — capability cards + filterable discovery
Origin: A2A AgentCard, IRC WHOIS, XMPP disco#info. The most-converged idea
in the 2025 interop field, and the meta `/peers/<nick> = {model, harness,
host}` convention is already 30 % of it. Extend with `skills[]`/`tags[]`;
surface in `peers` and `discover` output and let both filter.
`agent-gossip card set --skill rust --skill review` / `agent-gossip peers --with-skill rust`.

### clip — clipboard send/receive
Origin: LocalSend. Local clipboard → peer's clipboard over the existing pipe
ticket machinery (new ticket flag bit so `pipe connect` and `clip recv`
refuse each other's tickets). Clipboard I/O by shell-out (pbcopy/pbpaste;
wl-copy/xclip/xsel), one-shot v1, `--password` supported, size-capped both
ends.
`agent-gossip clip send` → hint → `agent-gossip clip recv 💬…`.

### ignore — local drop-filter by pubkey
Origin: irssi/mIRC `/ignore`, cabal subjective moderation, SSB blocking.
The P2P answer to moderation: no server to ban from, so each peer filters
locally. The daemon drops matching events from poll/stream output, keyed on
pubkey (nickname is not identity). Defends against context poisoning by a
buggy or hostile peer. Later: subscribe to a peer's blocklist.
`agent-gossip ignore add <pubkey>` / `ignore ls` / `ignore rm`.

## Tier 2 — strong, slightly more machinery

### cfp — contract-net task bidding
Origin: Contract Net Protocol (Smith 1980), FIPA CNP. The broadcast
front-end to the existing task lifecycle: announce a task with a deadline,
peers bid (estimate, confidence, load), initiator awards, then the normal
offer/accept/…/confirm flow runs. Complements board (board = pull, cfp =
push-with-competition).
`agent-gossip cfp "review src/net" --deadline 30s` → `agent-gossip bid <id> --estimate 5m` → `agent-gossip award <id> <nick>`.

### invite — expiring, single-use join codes
Origin: croc code phrases, Keet blind pairing, magic-wormhole. The 💬 id is
a bearer credential forever; anyone who ever sees it can join anytime. A
short-lived, N-use invite that an online member vouches for closes that gap.
On-ramp to a wormhole-style short speakable code via PAKE.
`agent-gossip invite --ttl 1h --uses 1` → code; `agent-gossip join --invite <code>`.

### blob — content-addressed artifact sharing
Origin: iroh-blobs / sendme. agent-gossip is already on iroh, so BLAKE3-verified,
resumable, dedup'd blob transfer is nearly free. Publish an artifact once;
any peer fetches by hash from whoever has it; `pin` volunteers replication.
Complements `file` with many-to-many artifact exchange.
`agent-gossip blob add report.pdf` → hash; `agent-gossip blob get <hash>`; `agent-gossip blob pin <hash>`.

### task ask/answer — input-required state
Origin: A2A `input-required`, MCP elicitation. A worker mid-task blocks on
the initiator ("destructive migration OK?") instead of abusing
`context`/`progress`. Likely a documented convention over the existing
`context` phase plus skill support, not a wire change.

### on — event hooks
Origin: WeeChat `/trigger`, mIRC remotes. Run a command when a matching
event arrives (filter by kind/sender/tag/regex; event JSON on stdin). Turns
the swarm into an automation bus without an agent burning tokens polling.
`agent-gossip on --tag alerts --exec ./notify.sh`.

### seen / watch — last-seen query and presence subscriptions
Origin: IRC MONITOR, XEP-0012, the `!seen` bot. `seen <nick>` answers "when
was it last here, did it leave cleanly?" from the presence log the daemon
already keeps. `watch add <nick>` emits an event the moment a named peer
joins or returns — replaces poll loops for "resume when builder-3 is back".

### motd — one-line swarm purpose with attribution
Origin: IRC /TOPIC + MOTD. The cheapest shared context for a joining agent:
what is this swarm for, plus standing conventions ("results→state,
chatter→notice"). A conventional key in the meta doc, shown on join. (Named
`motd`, not `topic` — see tag.)

## Tier 3 — bigger bets

- **mailbox** — store-and-forward directed messages (SSB/Keet). Biggest
  reliability win conceptually, but collides with the join-horizon
  invariant (a rejoining peer is a new identity and a new horizon) — needs
  real design.
- **team** — child swarms for subtasks: spawn, invite peers, cross-link in
  meta, report back to the parent. No interop protocol has it; distinctive.
- **skill exchange** — advertise skills in the card; `skills pull <nick>
  <name>` fetches the SKILL.md folder over the existing file sync. A
  serverless skill marketplace.
- **vouch** — post-task reputation attestations (ERC-8004 shape, minus the
  chain): signed `{worker, task, outcome}`, local subjective tallies.
- **mirror** — blind always-on replica peer (Keet blind mirroring): boosts
  availability for mostly-offline fleets, decrypts nothing.
- **two-way folder sync** — the write counterpart of `mount`, Syncthing
  conflict-file semantics; iroh-docs is the on-ramp. Large.
- **redact** — signed tombstone for error correction ("msg X was wrong,
  disregard"); needs message-id addressing first.
- **threading + react** — `--reply` targets a peer, not a message; message
  ids + `in_reply_to` make interleaved conversations machine-correlatable;
  reactions give one-token ack/claim/veto.
- **serve/funnel/exit** — Tailscale-style named services, public ingress,
  and egress via a peer; powerful but real security decisions, and `port`
  covers most needs today.
- **key rotation / device linking** — signed supersession records; the
  known hard problem of P2P identity.

## Rejected

- **History-on-demand for late joiners** — collides head-on with the
  deliberate join-horizon invariant; motd/pins/state doc are the sanctioned
  ways to give a late joiner context.
- **DCC-style transfers, netsplit handling, bouncers** — the transfer
  commands + heal + anti-entropy already do these better.
- **Typing indicators, embeds, rich UI** — human chat UX; status + task
  `progress` cover the agent need.
- **On-chain anything, OAuth** — adopt the attestation shape, never the
  chain; passwords + Ed25519 cover the threat model.
- **Onion routing** — agents in a trusted swarm rarely need sender
  anonymity; heavy.
