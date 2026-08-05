#!/usr/bin/env bash
# End-to-end check: three real `agent-gossip` processes on one loopback mesh.
#
# Proves the migration end to end, not just that it links:
#   1. three separate OS processes mesh and see each other
#   2. broadcast reaches every peer
#   3. a directed frame reaches its addressee and NOT a bystander
#      (the 0b9f438 fix, hand-ported onto a diverged tree in Phase 1)
#   4. an artifact rides the blob-offload side-channel and is fetched back
#      (the ops::blob code restored in Phase 2 — its only e2e exercise)
set -uo pipefail

BIN="${BIN:?set BIN to the agent-gossip binary}"
RUN="$(mktemp -d)"
FAILED=0

log()  { printf '\n\033[1m== %s\033[0m\n' "$*"; }
pass() { printf '  \033[32mPASS\033[0m %s\n' "$*"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAILED=1; }

cleanup() {
  for nick in alice bob carol; do
    [ -f "$RUN/$nick.pid" ] && kill "$(cat "$RUN/$nick.pid")" 2>/dev/null
  done
  sleep 1
  for nick in alice bob carol; do
    [ -f "$RUN/$nick.pid" ] && kill -9 "$(cat "$RUN/$nick.pid")" 2>/dev/null
  done
}
trap cleanup EXIT

# Wait until $1 appears in file $2, up to $3 seconds.
wait_for() {
  local needle="$1" file="$2" secs="${3:-30}" i=0
  while [ "$i" -lt "$((secs * 4))" ]; do
    grep -q "$needle" "$file" 2>/dev/null && return 0
    sleep 0.25; i=$((i + 1))
  done
  return 1
}

log "1. start the creator (alice) on a private loopback mesh"
"$BIN" create --nickname alice --state-file "$RUN/alice.state" \
  > "$RUN/alice.log" 2>"$RUN/alice.err" &
echo $! > "$RUN/alice.pid"

wait_for '"event":"ready"' "$RUN/alice.log" 60 \
  || { fail "alice never became ready"; sed -n '1,40p' "$RUN/alice.log" "$RUN/alice.err"; exit 1; }

GOSSIP=$(grep '"event":"ready"' "$RUN/alice.log" | head -1 \
  | sed -E 's/.*"gossip":"([^"]+)".*/\1/')
[ -n "$GOSSIP" ] || { fail "could not parse the gossip id"; cat "$RUN/alice.log"; exit 1; }
pass "alice ready on $GOSSIP"

log "2. join two more peers (bob, carol) as separate processes"
for nick in bob carol; do
  "$BIN" join "$GOSSIP" --nickname "$nick" --state-file "$RUN/$nick.state" \
    > "$RUN/$nick.log" 2>"$RUN/$nick.err" &
  echo $! > "$RUN/$nick.pid"
done
for nick in bob carol; do
  wait_for '"event":"ready"' "$RUN/$nick.log" 60 \
    || { fail "$nick never became ready"; sed -n '1,40p' "$RUN/$nick.log" "$RUN/$nick.err"; exit 1; }
  pass "$nick ready"
done

log "3. the roster: every peer sees the other two"
sleep 3
for nick in alice bob carol; do
  roster=$("$BIN" peers --gossip "$GOSSIP" --nickname "$nick" 2>&1)
  others=$(for other in alice bob carol; do
    [ "$other" = "$nick" ] && continue
    echo "$roster" | grep -q "\"$other\"" && echo ok
  done | wc -l | tr -d ' ')
  if [ "$others" = "2" ]; then pass "$nick sees both peers"
  else fail "$nick sees $others/2 peers: $roster"; fi
done

log "4. ping (RTT round-trip between processes)"
# `ping` acks immediately and is fire-and-forget; the daemon emits `ping_report`
# on its OWN stream ~10s later, so assert there rather than on this stdout.
"$BIN" ping --gossip "$GOSSIP" --nickname alice > "$RUN/ping.ack" 2>&1
if wait_for '"event":"ping_report"' "$RUN/alice.log" 30; then
  pass "ping_report: $(grep '"event":"ping_report"' "$RUN/alice.log" | head -1 | head -c 220)"
else
  fail "no ping_report on alice's stream within 30s (ack was: $(cat "$RUN/ping.ack"))"
fi

log "5. broadcast reaches every peer"
"$BIN" a2a broadcast --gossip "$GOSSIP" --nickname alice --text "hello-everyone" >/dev/null 2>&1
sleep 3
for nick in bob carol; do
  if "$BIN" poll --gossip "$GOSSIP" --nickname "$nick" 2>&1 | grep -q "hello-everyone"; then
    pass "$nick received the broadcast"
  else
    fail "$nick did not receive the broadcast"
  fi
done

log "6. directed frame reaches its addressee and NOT the bystander"
"$BIN" a2a msg --gossip "$GOSSIP" --nickname alice --to bob --text "for-bob-only" >/dev/null 2>&1
sleep 5
if "$BIN" poll --gossip "$GOSSIP" --nickname bob 2>&1 | grep -q "for-bob-only"; then
  pass "bob (the addressee) received the directed message"
else
  fail "bob did not receive the directed message"
fi
# The 0b9f438 property. Poll carol twice with a gap so an anti-entropy round
# (which is what used to leak the frame) has fired in between.
sleep 8
if "$BIN" poll --gossip "$GOSSIP" --nickname carol 2>&1 | grep -q "for-bob-only"; then
  fail "carol (a bystander) saw a frame directed at bob — the confinement fix regressed"
else
  pass "carol never saw the frame directed at bob, including after anti-entropy"
fi

log "7. blob offload: an artifact too large to inline"
# `a2a artifact` is the *worker* returning a task result, so a task must exist
# and its id is assigned by the worker and returned in the call response — the
# initiator does not get to pick it. The file is far above the inline body
# ceiling, so it cannot ride a gossip frame and must take the blob ALPN: the
# code restored in Phase 2, and this is its only end-to-end exercise.
"$BIN" a2a broadcast --gossip "$GOSSIP" --nickname bob --text "warmup-back" >/dev/null 2>&1
sleep 2
head -c 3000000 /dev/urandom > "$RUN/payload.bin"

# Task creation seals the request to bob's card, which propagates through the
# `meta` doc asynchronously — wait for it, or the request is dropped silently
# and the call just hangs.
for _ in $(seq 40); do
  "$BIN" meta --gossip "$GOSSIP" --nickname alice 2>/dev/null \
    | grep -q '"card"' && break
  sleep 0.5
done
"$BIN" a2a call --gossip "$GOSSIP" --nickname alice --to bob \
  --method SendMessage --text "render the report" --timeout-secs 45 \
  > "$RUN/call.json" 2>&1
TASKID=$(python3 -c "
import json,sys
for line in open('$RUN/call.json'):
    try: doc = json.loads(line)
    except Exception: continue
    tid = doc.get('result', {}).get('task', {}).get('id')
    if tid: print(tid); break
" 2>/dev/null)

if [ -z "$TASKID" ]; then
  fail "no task id from a2a call: $(head -c 400 "$RUN/call.json")"
else
  pass "alice opened task ${TASKID:0:8}… on bob"
  if "$BIN" a2a artifact --gossip "$GOSSIP" --nickname bob --task-id "$TASKID" \
       --file "$RUN/payload.bin" --file-name payload.bin > "$RUN/artifact.json" 2>&1; then
    pass "bob returned a 3 MB file result (offloaded over the blob channel)"
    sleep 5
    # The initiator must surface it as a fetchable ticket, never as inline bytes.
    "$BIN" poll --gossip "$GOSSIP" --nickname alice > "$RUN/alice.poll.json" 2>/dev/null
    # The artifact body is sealed on the wire and surfaces as JSON nested inside
    # a JSON string, so walk the whole structure (re-parsing embedded strings)
    # for the `url` key rather than pattern-matching the text.
    # Target the artifact part exactly. A looser search finds the agent cards'
    # `supportedInterfaces[].url` first, which are hex key material, not tickets.
    TICKET=$(python3 -c "
import json, sys

def artifact_urls(node):
    if isinstance(node, dict):
        parts = node.get('payload', {}).get('artifact', {}).get('parts')
        if isinstance(parts, list):
            for part in parts:
                url = part.get('url') if isinstance(part, dict) else None
                if url:
                    yield url
        for value in node.values():
            yield from artifact_urls(value)
    elif isinstance(node, list):
        for item in node:
            yield from artifact_urls(item)
    elif isinstance(node, str) and node[:1] in '{[':
        try:
            yield from artifact_urls(json.loads(node))
        except Exception:
            pass

for line in open('$RUN/alice.poll.json'):
    try:
        doc = json.loads(line)
    except Exception:
        continue
    for url in artifact_urls(doc):
        print(url)
        sys.exit(0)
" 2>/dev/null)
    if [ -n "$TICKET" ]; then
      pass "alice surfaced a blob ticket, not inline bytes (${TICKET:0:12}…)"
      if "$BIN" a2a fetch "$TICKET" --output "$RUN/fetched.bin" > "$RUN/fetch.json" 2>&1; then
        if cmp -s "$RUN/payload.bin" "$RUN/fetched.bin"; then
          pass "a2a fetch pulled it back byte-identical ($(wc -c < "$RUN/fetched.bin" | tr -d ' ') bytes)"
        else
          fail "fetched blob differs from the original"
        fi
      else
        fail "a2a fetch failed: $(head -c 300 "$RUN/fetch.json")"
      fi
    else
      fail "alice surfaced no blob ticket"
    fi
  else
    fail "artifact offload failed: $(head -c 400 "$RUN/artifact.json")"
  fi
fi

log "8. clean shutdown"
for nick in carol bob alice; do
  "$BIN" leave "$GOSSIP" --nickname "$nick" >/dev/null 2>&1 && pass "$nick left" || fail "$nick failed to leave"
done

echo
if [ "$FAILED" = 0 ]; then
  printf '\033[32mE2E: all checks passed\033[0m\n'
else
  printf '\033[31mE2E: failures above\033[0m — logs in %s\n' "$RUN"
  trap - EXIT   # keep the run dir for inspection
fi
exit "$FAILED"
