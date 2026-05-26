# Distributed end-to-end test runbook

A reusable procedure for exercising `ahs` across a **real multi-machine
fleet** — not the in-process or single-host subprocess suite, but actual
separate hosts on separate networks. Use it to reproduce and debug
cross-machine behavior (relay re-home, partition heal, beacon migration, a CPU
runaway, a memory leak) that only manifests with real OS processes, real
relays, and real network paths.

One machine is the **orchestrator** (the one you run this from). It owns the
source of truth, seeds every other host, launches the swarm, and collects
evidence. Each host **builds its own `ahs`** — no cross-compilation, no binary
shipping.

The commands below are parameterized over a `HOSTS` array; set it once and
paste each block.

```bash
HOSTS=(nyc-pi2 nyc-pi4 nyc-macmini1)   # every host EXCEPT this orchestrator
SRC=/Users/caiogondim/Developer/personal/agent-habilis/agent-swarm
DST='~/Developer/personal/agent-habilis/swarm'   # remote checkout dir
```

## 1. Prerequisites

- **Passwordless SSH** from the orchestrator to every host in `HOSTS`. One-time
  per host:
  ```bash
  ssh-copy-id -o StrictHostKeyChecking=accept-new -i ~/.ssh/id_ed25519.pub <host>
  ssh -o BatchMode=yes <host> 'echo OK'   # verify
  ```
- **Rust toolchain + `git` on every host** — each builds its own `ahs`. Both
  are already installed on the fleet (via Homebrew: `/opt/homebrew` on macOS,
  `/home/linuxbrew/.linuxbrew` on Linux). A **non-login SSH shell doesn't
  source the brew env**, so `cargo` isn't on `PATH` by default; the build step
  activates it with `brew shellenv` (the snippet below is host-agnostic).
- **Arch is irrelevant to this runbook** because each host builds natively
  (e.g. aarch64 Linux Pis + an arm64 Darwin Mac mini all build their own
  binary). No target triples, no cross toolchains.

## 2. Seed the code (orchestrator → every host)

Sync the working tree to `$DST` on each host. Include `.git` (the version
stamp is derived from it — see step 3) and exclude the multi-GB `target/`:

```bash
for h in "${HOSTS[@]}"; do
  ssh "$h" "mkdir -p $DST"
  rsync -az --delete --exclude target/ --exclude '*.log' "$SRC/" "$h:$DST/"
done
```

`--delete` keeps the remote tree an exact mirror (stale files removed).
`.git` is **deliberately not excluded**: `build.rs` stamps the git short hash +
dirty flag into the binary, so shipping `.git` is what makes every host report
the orchestrator's exact commit with `dirty:false`.

## 3. Build on each machine

Build on every host **and on the orchestrator itself** — the orchestrator runs
`ahs` too (a `create` plus its own joins, see step 4), so it must carry the
same freshly-stamped binary, not a stale earlier install.

```bash
# Host-agnostic brew activation: picks /home/linuxbrew or /opt/homebrew.
BREW_ENV='eval "$($(ls /home/linuxbrew/.linuxbrew/bin/brew /opt/homebrew/bin/brew 2>/dev/null | head -1) shellenv)"'

# Orchestrator (this machine) — local, no ssh:
( cd "$SRC" && cargo task install )

# Every other host:
for h in "${HOSTS[@]}"; do
  echo "=== building on $h ==="
  ssh "$h" "$BREW_ENV; cd $DST && cargo task install"
done
```

`cargo task install` runs `cargo install --path .` → `~/.cargo/bin/ahs` (a
release build; expect several minutes on a low-core Pi). Note the orchestrator
ends up with **two** `ahs` paths — `$SRC/target/release/ahs` and the installed
`~/.cargo/bin/ahs`; either works as long as `--version` shows the right commit.

**Verify every node runs the same build** — this is the whole point of shipping
`.git`:

```bash
for h in "${HOSTS[@]}"; do printf '%-14s ' "$h:"; ssh "$h" '~/.cargo/bin/ahs --version'; done
"$SRC/target/release/ahs" --version 2>/dev/null || (cd "$SRC" && cargo run -q -- --version)
```

All lines must print the **same** `0.x.y (<hash> dirty:false)`. A differing
hash means that host's tree drifted (re-run step 2); a `dirty:true` means the
seeded tree has uncommitted edits relative to its `.git`.

## 4. Launch the swarm

**Orchestrator runs `create`** (one instance) and yields the `ahs…` id. Use
whichever lookup flags the scenario needs (`--public` = all lookups on):

```bash
mkdir -p /tmp/ahs-e2e
ahs create --public --no-interactive --output json \
  | tee /tmp/ahs-e2e/orchestrator.out &
# Read the swarm id from the ready event:
SWARM=$(grep -m1 '"event":"ready"' /tmp/ahs-e2e/orchestrator.out | sed 's/.*"swarm":"\([^"]*\)".*/\1/')
echo "SWARM=$SWARM"
```

**Each host runs 2 `join` instances** of that id, distinct nicknames,
backgrounded:

```bash
for h in "${HOSTS[@]}"; do
  for n in 1 2; do
    ssh "$h" "nohup ~/.cargo/bin/ahs join $SWARM --nickname ${h}-$n \
      --no-interactive --output json > /tmp/ahs-${h}-$n.out 2>&1 &"
  done
done
```

**The orchestrator also runs 2 `join` instances** of its own swarm — same
pattern, locally (no `ssh`), so this machine carries a `create` plus two joins
just like every other host:

```bash
for n in 1 2; do
  nohup ahs join "$SWARM" --nickname orchestrator-$n \
    --no-interactive --output json > /tmp/ahs-e2e/orchestrator-join-$n.out 2>&1 &
done
```

Total nodes = **1** (orchestrator `create`) **+ 2 × (len(HOSTS) + 1)** joins
(the `+ 1` is the orchestrator's own two joins).

**Sanity check the mesh formed across machines** — ping from the orchestrator
and wait ~10s for the report:

```bash
ahs ping --swarm "$SWARM" --nickname <your-create-nick>
# ~10s later a ping_report on the orchestrator's --output json stream
# should list every <host>-1 / <host>-2 nickname plus orchestrator-1/-2.
```

`responded == known == 2×(len(HOSTS)+1)` means full reachability.

## 5. Debugging

### Where the logs are

Each member writes one always-on file, truncated on daemon start, named with
the swarm-prefix + nickname stem (same stem as its `.sock`):

- **Linux:** `/tmp/agent-habilis/swarm/logs/`
- **macOS:** the `agent-habilis/swarm/logs` subdir of the per-user temp dir
  (`$TMPDIR/...`).

Print the dir on any host with `ssh $h 'source ~/.cargo/env && cd $DST && cargo
task logs'`. **Every line is prefixed with the build version**, so interleaved
or post-rotation logs still self-identify their commit.

### What's already at `info` (always-on, even in release)

The operational subsystems (`gossip`, `lookup`, `beacon`, `lifecycle`,
`directory`) are pinned to `info` regardless of build profile. Watch a host's
flap/partition story live:

```bash
LOG=$(ssh "$h" 'ls -t /tmp/agent-habilis/swarm/logs/*.log | head -1')
ssh "$h" "tail -F $LOG" | grep -E \
  'neighbor up|neighbor down|reclaim|connect-probe finished|heal tick|mesh census'
```

| Signal | Line | Reading |
|---|---|---|
| Link flap | `gossip neighbor up` / `... down` (`endpoint_id`, `is_rendezvous`, `conn`, `relay`) | count up/down pairs per minute |
| Isolation | `armed fast reclaim window` (`reason=rendezvous-loss\|last-peer`) | beacon loss vs last-peer loss |
| Re-home failure | `rendezvous connect-probe finished connected=false` (`elapsed_ms`, `addr`) | rendezvous unreachable |
| Heal activity | `heal tick: re-probe + re-graft` (every 15s) | the recovery primitive firing |
| Mesh census | `mesh census` (`roster_len`, `link_len`, `meshed`; every 10s) | flap rate / link count as a direct time series |
| Silent partition | `neighbor census: silent partition` (`warn`) | meshed yet zero links — the post-freeze signature |

### Turn up detail without rebuilding

`RUST_LOG` always wins over the defaults. Relaunch an instance with a louder
filter to get heal internals, successful probes, anti-entropy, and the per-peer
`direct/relay/mixed` census breakdown:

```bash
ssh "$h" "RUST_LOG='agent_habilis_swarm::{gossip,lookup,beacon,lifecycle}=trace' \
  nohup ~/.cargo/bin/ahs join $SWARM --nickname ${h}-dbg \
  --no-interactive --output json > /tmp/ahs-${h}-dbg.out 2>&1 &"
```

(Subsystem → target table is in the root `AGENTS.md` "Logging" section.)

### CPU attribution — required for a runaway

**Logs show the flap symptom but cannot localize a CPU burn.** A tight
`select!`/retry loop spinning without backoff prints nothing. To find the loop
eating the cores you must sample the live process. First find the busy pid:

```bash
ssh "$h" 'ps aux | sort -nrk3 | head'   # %CPU descending
```

Then sample it:

- **Linux:**
  ```bash
  ssh "$h" "perf record -F 999 -p <pid> -g -- sleep 10 && perf report --stdio | head -60"
  # or live: ssh "$h" 'perf top -p <pid>'
  ```
  No `perf`? fall back to repeated kernel-stack reads:
  ```bash
  ssh "$h" 'for i in $(seq 20); do cat /proc/<pid>/stack 2>/dev/null; echo ---; sleep .5; done'
  ```
- **macOS:**
  ```bash
  ssh "$h" "sample <pid> 10 -file /tmp/ahs-sample.txt && head -100 /tmp/ahs-sample.txt"
  ```

The hot stack frames name the loop burning the cores.

### Memory leak check

**RSS** = Resident Set Size: the physical RAM a process currently has mapped in
(not swap, not virtual address space) — the `RSS` column from `ps`/`top`,
reported in kilobytes by `ps -o rss=`. Sample it over time (a known runaway
leaked to ~1.5–1.75 GB):

```bash
ssh "$h" 'for i in $(seq 30); do ps -o rss= -p <pid>; sleep 10; done'
```

A healthy node holds a roughly flat RSS once its roster and message log settle
(the log is a bounded ring — default 1000 messages — so steady traffic does not
grow it). A **monotonically climbing RSS while the roster stays the same size**
is therefore a leak: memory retained that should have been freed. Watch the
*trend*, not the absolute number — `ps` RSS counts shared library pages in each
process's total, so it slightly over-reports and wobbles; the upward slope is
the signal. If that slope tracks the NeighborUp/Down churn (`mesh census` /
flap rate in the log), the leak is in the connection-flap path rather than in
message handling.

### Collect evidence back to the orchestrator

```bash
mkdir -p ./evidence
for h in "${HOSTS[@]}"; do
  rsync -az "$h:/tmp/agent-habilis/swarm/logs/" "./evidence/$h-logs/" 2>/dev/null
  scp "$h:/tmp/ahs-${h}-"*.out "./evidence/" 2>/dev/null
done
```

## 6. Teardown

```bash
for h in "${HOSTS[@]}"; do ssh "$h" "pkill -f 'ahs (create|join)'"; done
# Orchestrator: stop its own create + two joins:
pkill -f 'ahs (create|join)'
```

`pkill -f` is scoped to `ahs create|join` so a co-located unrelated `ahs`
process (another agent's run, an MCP server) is never touched. Confirm with
`ssh $h 'pgrep -af "ahs (create|join)"'` returning nothing.
