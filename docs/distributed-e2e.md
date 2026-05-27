# Distributed end-to-end test runbook

A reusable procedure for exercising `ahs` across a **real multi-machine
fleet** — not the in-process or single-host subprocess suite, but actual
separate hosts on separate networks. Use it to reproduce and debug
cross-machine behavior (relay re-home, partition heal, beacon migration, a CPU
runaway, a memory leak) that only manifests with real OS processes, real
relays, and real network paths.

One machine is the **orchestrator** (the one you run this from). It is a
**pure driver — it runs NO `ahs` instance of its own** (no `create`, no
`join`). It owns the source of truth, seeds every other host, drives the run
over SSH, and collects evidence. Keeping it out of the swarm means a swarm-side
bug (CPU runaway, leak, OOM) can never take down the machine you're driving
from. Each host **builds its own `ahs`** — no cross-compilation, no binary
shipping.

The fleet is the `HOSTS` array. One host is the **`CREATE_HOST`** (it runs the
single `create`); every host runs its joins. macOS coverage comes from
**`nyc-macmini1`** (the macOS host in the fleet), not the orchestrator.

```bash
HOSTS=(nyc-pi2 nyc-pi4 slz-pi1 nyc-macmini1)   # the swarm fleet (orchestrator NOT included)
CREATE_HOST=nyc-macmini1                        # runs `create` (macOS beacon coverage)
SRC=/Users/caiogondim/Developer/personal/agent-habilis/agent-swarm
DST='~/Developer/personal/agent-habilis/swarm'   # remote checkout dir
```

> The orchestrator builds/runs no `ahs` — skip it in the build step (§3) and
> never launch `create`/`join` locally. Sanity checks (`ping`/`poll`) run **on
> a host** via SSH, since they need a local daemon socket.

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

Build on **every host in `HOSTS`** — *not* on the orchestrator (it runs no
`ahs`, so it needs no binary). Each host gets a freshly-stamped release build.

```bash
# Host-agnostic brew activation: picks /home/linuxbrew or /opt/homebrew.
BREW_ENV='eval "$($(ls /home/linuxbrew/.linuxbrew/bin/brew /opt/homebrew/bin/brew 2>/dev/null | head -1) shellenv)"'

for h in "${HOSTS[@]}"; do
  echo "=== building on $h ==="
  ssh "$h" "$BREW_ENV; cd $DST && cargo task install"
done
```

`cargo task install` runs `cargo install --path .` → `~/.cargo/bin/ahs` (a
release build; expect several minutes on a low-core Pi).

> **Low-RAM hosts may OOM on the fat-LTO link.** The release profile uses
> `lto=fat`, whose final `rustc` link of the iroh tree can exceed ~8 GB and get
> OOM-killed on a small host (e.g. a 8 GB Pi like `slz-pi1`). Build those with
> LTO off — functionally identical, just less optimized (irrelevant to the
> flap/leak/CPU behavior under test):
> ```bash
> ssh slz-pi1 "$BREW_ENV; cd $DST && \
>   CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 cargo task install"
> ```

> **Gotcha — re-seeding an existing checkout leaves a stale version stamp.**
> `rsync -a` (step 2) preserves the *source* mtimes, so after re-seeding a host
> that already built once, cargo sees `.git` as "unchanged" and **reuses the
> cached `VERGEN_GIT_SHA`** — the rebuild compiles the new code but
> `ahs --version` still prints the *old* commit. Bust the mtime cache before
> rebuilding so vergen re-stamps:
> ```bash
> ssh "$h" "cd $DST && touch .git/HEAD .git/refs/heads/* && \
>   find . -name '*.rs' -not -path './target/*' -exec touch {} +"
> ```
> (Or seed with `rsync -a --no-times` so files land with fresh mtimes.) A first
> build on a clean host doesn't need this.

**Verify every node runs the same build** — this is the whole point of shipping
`.git`:

```bash
for h in "${HOSTS[@]}"; do printf '%-14s ' "$h:"; ssh "$h" '~/.cargo/bin/ahs --version'; done
```

All hosts must print the **same** `0.x.y (<hash> dirty:false)`. A differing
hash means that host's tree drifted (re-run step 2); a `dirty:true` means the
seeded tree has uncommitted edits relative to its `.git` (expected when running
an unreleased fix — fine as long as the hash + dirty flag match across hosts).
The orchestrator has no `ahs` to check.

## 4. Launch the swarm

Everything launches **over SSH from the orchestrator** — the orchestrator runs
no `ahs` itself.

**`CREATE_HOST` runs `create`** (one instance) and yields the `ahs…` id; the
orchestrator reads it back over SSH. Use whichever lookup flags the scenario
needs (`--public` = all lookups on):

```bash
ssh "$CREATE_HOST" "mkdir -p /tmp/ahs-e2e; nohup ~/.cargo/bin/ahs create --public \
  --no-interactive --output json > /tmp/ahs-e2e/create.out 2>&1 &"
# Read the swarm id (and the creator's nick, for the ping below) from CREATE_HOST:
for i in $(seq 1 40); do ssh "$CREATE_HOST" 'grep -q ready /tmp/ahs-e2e/create.out' && break; sleep 1; done
READY=$(ssh "$CREATE_HOST" "grep -m1 '\"event\":\"ready\"' /tmp/ahs-e2e/create.out")
SWARM=$(echo "$READY" | sed 's/.*"swarm":"\([^"]*\)".*/\1/')
CREATOR_NICK=$(echo "$READY" | sed 's/.*"nickname":"\([^"]*\)".*/\1/')
echo "SWARM=$SWARM  CREATOR_NICK=$CREATOR_NICK"
```

**Each host runs 2 `join` instances** of that id, distinct nicknames,
backgrounded (the `CREATE_HOST` carries its `create` plus these two joins):

```bash
for h in "${HOSTS[@]}"; do
  for n in 1 2; do
    ssh "$h" "nohup ~/.cargo/bin/ahs join $SWARM --nickname ${h}-$n \
      --no-interactive --output json > /tmp/ahs-${h}-$n.out 2>&1 &"
  done
done
```

Total nodes = **1** (`create` on `CREATE_HOST`) **+ 2 × len(HOSTS)** joins. The
orchestrator contributes **zero** nodes.

> **Cap every node so a runaway can never crash the host.** The 2026-05-26 soak
> took the orchestrator MacBook *down* (hard reboot) because its joins hit a CPU
> runaway + memory leak with **no OS resource limit** — a leaking daemon must
> cost a killed process, never a dead machine. Launch every `ahs` under a cap:
>
> - **Linux hosts (cgroup, hard):** wrap each `ahs` in a transient scope —
>   ```bash
>   systemd-run --user --scope -p MemoryMax=1G -p MemorySwapMax=0 -p CPUQuota=150% \
>     ~/.cargo/bin/ahs join "$SWARM" --nickname ${h}-$n --no-interactive --output json ...
>   ```
>   The kernel OOM-kills the scope at `MemoryMax` and throttles it at `CPUQuota`;
>   the host stays up. (`--user` so no root needed.)
> - **macOS host (`nyc-macmini1`, no cgroups):** there is no reliable
>   per-process RSS cap, so rely on **(a)** the daemon's built-in
>   `AHS_RSS_WARN_MB` warn (below) and **(b)** the sampler kill-switch in §7.2,
>   which `kill`s any `ahs` whose sampled RSS exceeds a cap.
>
> The orchestrator itself runs no `ahs`, so it is never at risk regardless —
> that's the point of keeping it out of the swarm. Set `AHS_RSS_WARN_MB` on
> **every** node (default 1024) so each daemon logs a one-shot `warn` + JSON
> `info` event the moment its own RSS crosses the soft threshold.

**Sanity check the mesh formed across machines** — fire a ping **on the
`CREATE_HOST`** (the orchestrator has no daemon to ping from) and read its
report back:

```bash
ssh "$CREATE_HOST" "ahs ping --swarm $SWARM --nickname $CREATOR_NICK"
# ~10s later a ping_report lands on CREATE_HOST's create.out --output json stream:
sleep 12; ssh "$CREATE_HOST" "grep '\"event\":\"ping_report\"' /tmp/ahs-e2e/create.out | tail -1"
# should list every <host>-1 / <host>-2 nickname.
```

`responded == known == 2×len(HOSTS) − 1` (every join except the creator's own
node answering itself) means full reachability.

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

> ⚠️ **ALWAYS tear down the fleet when a run ends — every time, no exceptions,
> including interrupted, crashed, or "I'll get to it later" runs.** Leftover
> `create`/`join` daemons keep **leaking memory and churning connections** for as
> long as they run (a forgotten run once leaked for ~1h and an earlier one
> crashed a host); leftover `ahs-soak` **samplers and stress drivers** keep
> generating load and corrupting the next run's metrics. **The very first step
> after finishing — or abandoning — a run is this teardown.** Never start a new
> run, walk away, or consider the work done until `pgrep` returns nothing on
> *every* host.

```bash
# Kill the daemons AND the soak helpers (samplers, stress drivers, kill-switch
# guards — all live under ~/ahs-soak) on every host and the orchestrator.
for h in "${HOSTS[@]}"; do ssh "$h" "pkill -9 -f 'ahs (create|join)'; pkill -9 -f ahs-soak"; done
pkill -9 -f 'ahs (create|join)'; pkill -9 -f ahs-soak   # orchestrator

# CONFIRM nothing is left — must print 0 for every host and the orchestrator:
for h in "${HOSTS[@]}"; do printf '%-14s ' "$h:"; ssh "$h" "pgrep -f '[a]hs (create|join)' | wc -l"; done
printf '%-14s ' orchestrator:; pgrep -f '[a]hs (create|join)' | wc -l
```

`pkill -f 'ahs (create|join)'` is scoped so a co-located unrelated `ahs`
(another agent's run, an MCP server) is never touched; the separate
`pkill -f ahs-soak` clears the §7.2 samplers / stress / guards. **Do not skip
the confirmation pass** — a launch that isn't verified-torn-down is an
unfinished run.

## 7. Long-run soak (overnight)

A multi-hour soak confirms a fix *holds over time* — no delayed flap storm,
**flat RSS (no slow leak)**, stable mesh — beyond what a 20-minute run shows.
The harshest member is a **geographically distant, NAT'd** node (e.g. a Pi in
Brazil, ~100-200ms cross-continent RTT over relay) — the unstable-path case that
provokes the membership churn. Build + launch as in §1-4, but **detached** so it
survives the orchestrator sleeping or the driving session ending: the always-on
hosts carry the swarm (creator-independent + beacon migration), so the
orchestrator's own sleep/wake is just bonus heal data.

### 7.1 Launch detached

Use `nohup … &` for `create` and every `join` (not foreground), teeing
`--output json` to per-node files. Capture the swarm id from the `ready` line as
in §4. The swarm must be `--public` (the relay path is the point).

### 7.2 Time-series metric samplers

The per-member logs carry the flap / `mesh census` / heal story at `info`, but
**not CPU/RSS over time** — the leak signal. Add a detached per-host sampler
that survives disconnect:

```bash
SAMPLER='mkdir -p ~/ahs-soak; nohup sh -c '\''while true; do ts=$(date +%s);
  ps -eo pid,pcpu,rss,etime,args | grep "[a]hs join\|[a]hs create" |
  while read pid cpu rss et rest; do echo "$ts,$pid,$cpu,$rss,$et" >> ~/ahs-soak/metrics.csv; done;
  sleep 60; done'\'' >/dev/null 2>&1 &'
for h in "${HOSTS[@]}"; do ssh "$h" "$SAMPLER"; done
# (No local sampler — the orchestrator runs no ahs.)
```

**Sampler kill-switch (the macOS host-safety net).** Where cgroups aren't
available (macOS), add a second detached loop that `kill`s any `ahs` whose RSS
crosses a hard cap (`RSS_KILL_KB`, e.g. 2 GiB) — so a leak is bounded to a dead
*process*, not a dead host, exactly as the 2026-05-26 crash demands. The killed
daemon's swarm is creator-independent, so the mesh survives and the incident is
captured in `metrics.csv` and the per-member log:

```bash
RSS_KILL_KB=2097152   # 2 GiB
GUARD='nohup sh -c '\''while true; do
  ps -eo pid,rss,args | grep "[a]hs join\|[a]hs create" |
  while read pid rss rest; do [ "$rss" -gt '"$RSS_KILL_KB"' ] &&
    { echo "$(date +%s) KILL pid=$pid rss_kb=$rss" >> ~/ahs-soak/kills.log; kill "$pid"; }; done;
  sleep 30; done'\'' >/dev/null 2>&1 &'
for h in "${HOSTS[@]}"; do ssh "$h" "$GUARD"; done
# (No local guard — the orchestrator runs no ahs.)
```

(On Linux the §4 `systemd-run` `MemoryMax` already enforces this in-kernel; the
guard is a portable backstop and the only mechanism on macOS. Tear it down with
`pkill -f ahs-soak` alongside the sampler — both match that pattern.)

### 7.3 Live 30-minute report cycle

If a session drives the soak, report every 30 min (the samplers are the durable
record between/after reports). Each cycle:

1. **Exchange a probe message**, rotating the sender across the fleet (so every
   origin, including the cross-continent node, is exercised):
   `ahs msg --swarm $SWARM --nickname <sender> --text "soak-probe <seq> <ts>"`,
   wait ~8s, then `ahs poll` each node and confirm the probe arrived —
   **msgs replicated: N/(node count)**.
2. **Report**, per node:
   - **RSS** — *the* leak check; flat = bounded-memory holds (see §5 memory
     check for how to read the trend).
   - **CPU %** — must stay low (a sustained climb is the runaway returning).
   - **Log size** (bytes + lines) — growth rate proxies flap activity and flags
     disk-fill risk.
   - **Msgs replicated** — N/(node count) saw the probe (end-to-end delivery).
   - **Mesh health** — `ahs ping` RTT + `responded/known`; latest `mesh census
     roster_len/link_len/meshed`.
   - **Flap delta** — new `neighbor up`/`down` since the last report.
   - **Stability** — new `warn`/`error`, beacon migrations, reclaim arming.
   - **Liveness** — every daemon still up (no crash/restart).

### 7.4 Morning analysis + teardown

```bash
mkdir -p ./soak-evidence
for h in "${HOSTS[@]}"; do
  rsync -az "$h:~/ahs-soak/metrics.csv" "./soak-evidence/$h-metrics.csv"
  rsync -az "$h:/tmp/agent-habilis/swarm/logs/" "./soak-evidence/$h-logs/" 2>/dev/null
done
```

Per node: **RSS first→last slope** (the leak verdict — must be flat), peak/avg
**CPU**, **flap rate** bucketed by hour (`grep -c "neighbor up/down"` + the
`mesh census` time series), and **stability events** (beacon migrations, reclaim
arming, `warn`/`error`). Cross-reference the RSS slope against the flap rate — a
pre-fix leak tracked the connection churn. Then tear down per §6, and also kill
the samplers: `for h in "${HOSTS[@]}"; do ssh "$h" "pkill -f ahs-soak"; done`
(nothing to kill locally — the orchestrator runs no `ahs`).
