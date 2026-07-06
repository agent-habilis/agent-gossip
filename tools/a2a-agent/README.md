# a2a-agent — vanilla A2A agents for bridge testing

A minimal, dependency-free ([Bun](https://bun.sh) runtime only, TypeScript)
[A2A](https://a2a-protocol.org) agent used to prove `agent-square a2a` bridges two real
agents transparently.

`agent.ts` is both roles:

```sh
# Agent A — an A2A echo server that serves an Agent Card + answers message/send
bun agent.ts server --port 9999 --name origin-agent

# Agent B — an A2A client that discovers via the card and sends a message
bun agent.ts client --url http://127.0.0.1:9999
```

It speaks raw HTTP/JSON-RPC (no SDK, just `Bun.serve` + `fetch`) so it exercises
real A2A discovery: the client reads the Agent Card's absolute `url` and posts
there — exactly the field `agent-square a2a connect` rewrites to the local bridge so the
tunnel is transparent.

## End-to-end bridge test

`tools/a2a-bridge-test.ts` runs the whole flow hermetically on one machine:

```
agent A (server) ──▶ agent-square a2a expose ──mesh──▶ agent-square a2a connect ──▶ agent B (client)
```

Both `agent-square` sides use the hidden `--loopback` flag, so the ticket carries the
exposer's direct `127.0.0.1` address and no mDNS/DHT/relay is needed. The test
asserts agent B discovered the *local bridge* endpoint (card rewrite) and got an
echo reply back (round-trip).

```sh
bun tools/a2a-bridge-test.ts   # prints "PASS" and exits 0 on success
```

Requires `bun` and a working `cargo build`.
