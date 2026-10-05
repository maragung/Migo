#!/usr/bin/env bash
# Full-scale load runner — brief section 172's measured scenarios, nightly shape.
#
# tools/load/run.sh is the quick gate: one node, one shape, minutes. This script is
# the other half of section 172's contract — the full-scale list, run against one
# real release-build node with the metrics the brief names printed for every step:
#
#   1. ten thousand idle sessions        (loadgen `connect`, the idle shape)
#   2. a thousand messages per second    (loadgen `messaging`, senders x rate = the total)
#   3. one conversation of 256 members   (loadgen `fanout` — the honest ceiling: rooms cap
#                                         at 50 members, groups at 256; ten thousand is a
#                                         product change, not a runner limit)
#   4. a thousand concurrent calls       (loadgen `calls` — 500 pairs mid-call; the media
#                                         plane is P2P and never crosses the server)
#   5. a thousand voice-note uploads     (loadgen `voice-notes`, closed-loop)
#   6. mass sync after an outage         (loadgen `outage` — this script kills the node
#                                         mid-run and restarts it; the scenario's settle
#                                         phase demands every acknowledged message delivered
#                                         exactly once and every session resumed)
#
# Every step is deterministic pass/fail through loadgen's own exit codes (3 = error
# budget exceeded, 4 = wire-byte budget exceeded, 5 = the run never finished) and
# bounded in wall-clock by its --duration. The exit code alone is not the verdict,
# though, and this script used to treat it as one: a run whose event loop drains
# mid-flight writes no report and still exits 0, and a run that opens no session at
# all exits 0 because an error rate of zero over a denominator of zero is not an
# error. So every step's report is handed to `loadgen/dist/judge.js`, which holds it
# to what the step claims about itself — sessions opened, window held, operations
# measured — and a step whose report does not support a pass fails with exit 6
# whatever loadgen returned. Latency percentiles (p50/p95/p99) and bytes per user per minute are in
# each step's JSON report; the server-side families a client cannot measure — live
# sessions, dropped frames, reconnect outcomes, media bytes, and memory per session
# (VmRSS / sessions) — are read from the node's /metrics and /proc and printed per
# step. Wall-clock percentiles are reported for humans, never asserted, so a slow
# runner fails only by actually breaking.
#
# The node runs on PostgreSQL, not the in-memory store, because step 6 restarts it:
# accounts and messages must survive the kill for "mass sync after the outage" to be
# a statement about sync rather than about amnesia.
#
# Never point this at anything but a disposable node. Ten thousand registrations from
# one host is a flood by any server's standards; the node config below raises the
# anonymous rate tier accordingly, which is only honest against a node you own.
#
# Environment variables (all optional):
#   MIGOD_BIN       path to the migod binary (default: ../../server/target/release/migod)
#   NODE_PORT       HTTP/WS port for the node (default: 18200)
#   PG_HOST/PG_PORT/PG_USER/PG_PASSWORD   PostgreSQL for the node's store
#   PG_DATABASE     database name (default: migo_loadfull; created if missing)
#   IDLE_VUS / IDLE_DURATION / IDLE_CONNECT_CONCURRENCY   step 1
#   MSG_VUS / MSG_RATE / MSG_DURATION                      step 2 (senders x rate = msg/s)
#   FANOUT_VUS / FANOUT_RATE / FANOUT_DURATION             step 3 (<= 256: the group ceiling)
#   CALLS_VUS / CALLS_DURATION                             step 4 (pairs = VUs / 2)
#   VOICE_VUS / VOICE_DURATION                             step 5
#   OUTAGE_VUS / OUTAGE_DURATION / OUTAGE_KILL_AFTER       step 6
#   ERROR_BUDGET    loadgen --max-error-rate (default: 0.05)
#   KEEP_ALIVE      if set, leave the node running after the run
#   EVIDENCE_DIR    if set, the run's evidence — the RSS log, the per-step /metrics
#                   dumps, the node's own log, every step's JSON report — is copied
#                   there on the way out, however the run ends. CI sets it to
#                   tools/load/logs and uploads that path, so a nightly that fails on
#                   step 4 keeps the numbers that explain step 4 rather than only the
#                   sentence saying it failed.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

MIGOD_BIN="${MIGOD_BIN:-$REPO_ROOT/server/target/release/migod}"
NODE_PORT="${NODE_PORT:-18200}"

PG_HOST="${PG_HOST:-localhost}"
PG_PORT="${PG_PORT:-15432}"
PG_USER="${PG_USER:-migo}"
PG_PASSWORD="${PG_PASSWORD:-migo}"
PG_DATABASE="${PG_DATABASE:-migo_loadfull}"

# Step shapes. The defaults are the nightly contract: the brief's full-scale list,
# sized for a 2-core GitHub runner (one node process, one loadgen process, and
# nothing else on the machine). Each is bounded so the whole script is bounded.
IDLE_VUS="${IDLE_VUS:-10000}"
IDLE_DURATION="${IDLE_DURATION:-60s}"
IDLE_CONNECT_CONCURRENCY="${IDLE_CONNECT_CONCURRENCY:-200}"
MSG_VUS="${MSG_VUS:-40}"            # 20 pairs; the even half of each pair sends
MSG_RATE="${MSG_RATE:-50}"          # 20 senders x 50/s = the brief's 1000 msg/s
MSG_DURATION="${MSG_DURATION:-120s}"
FANOUT_VUS="${FANOUT_VUS:-256}"     # the group ceiling; more VUs stay idle by design
FANOUT_RATE="${FANOUT_RATE:-5}"
FANOUT_DURATION="${FANOUT_DURATION:-90s}"
CALLS_VUS="${CALLS_VUS:-1000}"      # 500 pairs, each holding one call mid-cycle
CALLS_DURATION="${CALLS_DURATION:-120s}"
VOICE_VUS="${VOICE_VUS:-1000}"      # one upload in flight per VU, closed-loop
VOICE_DURATION="${VOICE_DURATION:-120s}"
OUTAGE_VUS="${OUTAGE_VUS:-40}"      # 20 pairs streaming through the offline outbox
OUTAGE_DURATION="${OUTAGE_DURATION:-90s}"
OUTAGE_KILL_AFTER="${OUTAGE_KILL_AFTER:-20}"  # seconds of steady traffic before the kill
ERROR_BUDGET="${ERROR_BUDGET:-0.05}"
KEEP_ALIVE="${KEEP_ALIVE:-}"
EVIDENCE_DIR="${EVIDENCE_DIR:-}"
# A relative evidence path is taken from the repository root rather than from wherever this
# script was invoked: CI names `tools/load/logs` and uploads exactly that path from the root,
# and a run started by hand from tools/load has to land in the same place or the artifact is
# empty on the nights someone is watching most closely.
case "$EVIDENCE_DIR" in
  "" | /*) ;;
  *) EVIDENCE_DIR="$REPO_ROOT/$EVIDENCE_DIR" ;;
esac

if [ ! -x "$MIGOD_BIN" ]; then
  echo "migod binary not found at $MIGOD_BIN; build it with:" >&2
  echo "  (cd $REPO_ROOT/server && cargo build --release --bin migod)" >&2
  exit 1
fi

LOADGEN="$REPO_ROOT/tools/loadgen/dist/main.js"
JUDGE="$REPO_ROOT/tools/loadgen/dist/judge.js"
if [ ! -f "$LOADGEN" ] || [ ! -f "$JUDGE" ]; then
  echo "loadgen not built ($LOADGEN, $JUDGE); build it with:" >&2
  echo "  (cd $REPO_ROOT && make build-ts)" >&2
  exit 1
fi

# Ten thousand virtual users is ten thousand real SDK clients (keys, sockets, buffers)
# in one Node process; the default heap chokes well before the server does.
export NODE_OPTIONS="--max-old-space-size=4096"

WORK_DIR="$(mktemp -d -t migo-loadfull-XXXXXX)"
CONFIG_FILE="$WORK_DIR/node.toml"
NODE_LOG="$WORK_DIR/node.log"
RSS_LOG="$WORK_DIR/rss.log"
mkdir -p "$WORK_DIR/reports"

NODE_PID=""
LOADGEN_PID=""
RSS_MONITOR_PID=""

# The working directory is a mktemp one, so everything in it dies with the trap below: the
# RSS log, the per-step metrics dumps, the node's log, every step's JSON report. A run that
# fails keeps those or it keeps nothing — the step line says a step failed, and only the
# files say why. Collected inside the trap, so a run that dies mid-step still hands over
# the evidence up to the step it died in.
collect_evidence() {
  [ -n "$EVIDENCE_DIR" ] || return 0
  mkdir -p "$EVIDENCE_DIR"
  cp -f "$RSS_LOG" "$EVIDENCE_DIR/rss.log" 2>/dev/null || true
  cp -f "$NODE_LOG" "$EVIDENCE_DIR/node.log" 2>/dev/null || true
  cp -f "$WORK_DIR"/metrics-*.txt "$EVIDENCE_DIR/" 2>/dev/null || true
  cp -f "$WORK_DIR"/reports/*.json "$EVIDENCE_DIR/" 2>/dev/null || true
  echo "==> Evidence copied to $EVIDENCE_DIR"
}

cleanup() {
  # Before the teardown below removes the only copy.
  collect_evidence
  if [ -n "$RSS_MONITOR_PID" ]; then
    kill "$RSS_MONITOR_PID" 2>/dev/null || true
    wait "$RSS_MONITOR_PID" 2>/dev/null || true
  fi
  if [ -n "$LOADGEN_PID" ]; then
    kill "$LOADGEN_PID" 2>/dev/null || true
    wait "$LOADGEN_PID" 2>/dev/null || true
  fi
  if [ -z "$KEEP_ALIVE" ]; then
    echo "==> Tearing down the node"
    if [ -n "$NODE_PID" ]; then
      kill "$NODE_PID" 2>/dev/null || true
      wait "$NODE_PID" 2>/dev/null || true
    fi
    rm -rf "$WORK_DIR"
  else
    echo "==> KEEP_ALIVE set, leaving the node running (pid=$NODE_PID, log=$NODE_LOG)"
  fi
}
trap cleanup EXIT INT TERM

# One config file for the node's whole life, including the outage restart: the same
# environment must come back after the kill, or the restart is a different node.
# Rate limits are raised far above production because every step registers its VUs
# from one address — the anonymous tier would otherwise be the thing under test.
cat >"$CONFIG_FILE" <<EOF
[rate_limit]
user_burst = 50000
user_refill_per_second = 25000
anonymous_burst = 50000
anonymous_refill_per_second = 25000
bot_burst = 5000
bot_refill_per_second = 2500

[auth]
registration_cost = 1

# The same reasoning as the rate limits above, one layer down. Every step drives this node
# from one process, and the development pool of 16 is what a 200-way connect burst exhausts
# first: a handshake that waits acquire_timeout_ms for a connection answers
# STORAGE_UNAVAILABLE, and idle-10k counts a session that answered that as one that never
# opened. On 2026-10-03 that cost 12 of 10000 sessions on a runner about a fifth slower than
# the one that connected all ten thousand on 2026-09-21 with the same tree, so the pool —
# not the server's ability to hold ten thousand sessions — was the thing under test. 48 stays
# well under the Postgres container's own max_connections of 100.
[store]
max_connections = 48

[media]
local_dir = "$WORK_DIR/media"
EOF

start_node() {
  # Appends: across the outage restart both lives land in one log, in order.
  MIGO_CONFIG="$CONFIG_FILE" \
  MIGO_NODE__ID="load-node" \
  MIGO_NODE__ROLES=api,gateway,room,game \
  MIGO_NODE__ENVIRONMENT=development \
  MIGO_HTTP__BIND="127.0.0.1:$NODE_PORT" \
  MIGO_HTTP__PUBLIC_URL="http://localhost:$NODE_PORT" \
  MIGO_STORE__BACKEND=postgres \
  MIGO_STORE__URL="postgres://$PG_USER:$PG_PASSWORD@$PG_HOST:$PG_PORT/$PG_DATABASE" \
  MIGO_AUTH__TOKEN_KEY="development-only-insecure-token-key" \
  MIGO_AUTH__ALLOW_REGISTRATION=true \
  RUST_LOG=info \
  "$MIGOD_BIN" >>"$NODE_LOG" 2>&1 &
  NODE_PID=$!
  wait_health
}

wait_health() {
  for _ in $(seq 1 300); do
    if curl -fsS "http://localhost:$NODE_PORT/health" >/dev/null 2>&1; then
      echo "==> node healthy on :$NODE_PORT (pid $NODE_PID)"
      return 0
    fi
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
      echo "migod exited before becoming healthy; tail of log:" >&2
      tail -n 40 "$NODE_LOG" >&2
      return 1
    fi
    sleep 0.2
  done
  echo "node did not become healthy in time; tail of log:" >&2
  tail -n 40 "$NODE_LOG" >&2
  return 1
}

# The server-side families a client cannot measure, printed per step: live sessions,
# dropped frames (the brief's frame metric), reconnect outcomes, wire frames, media.
# Saved whole per step too, so a failed nightly keeps its evidence.
print_metrics() {
  local step="$1"
  local metrics_file="$WORK_DIR/metrics-$step.txt"
  if ! curl -fsS "http://localhost:$NODE_PORT/metrics" >"$metrics_file" 2>/dev/null; then
    echo "  (node /metrics unreachable after step '$step')" >&2
    return
  fi
  echo "--- node metrics after '$step' ---"
  # The session counters ride beside the gauge because the gauge cannot be read alone:
  # `sessions_live` is opened minus closed, so an idle step that asked for ten thousand
  # sessions and shows a peak of 2,960 live is either a node that never held them or a
  # pool that churned — a defect on one reading and a fact about the run on the other.
  # `sessions_opened_total` and `sessions_closed_total` are what separate the two, and
  # the arithmetic has to close: live = opened - closed, at every step.
  grep -E \
    '^migo_gateway_(sessions_live|sessions_opened_total|sessions_closed_total|frames_dropped_total|frames_in_total|frames_out_total|resume_total)' \
    "$metrics_file" || echo "  (no gateway metric lines)"
  grep -E '^migo_(reconnect_total|media_)' "$metrics_file" || true
}

# Memory per session (the brief's per-session metric): sample the node's VmRSS *and*
# the sessions it reports live, at the same moment, while the idle pool is up.
#
# Both columns come from the same sample because the figure is a ratio and the two halves
# have to describe the same instant: reading VmRSS during the run and sessions_live after
# it — which is what this did — divides a peak by a pool that loadgen has already torn
# down, so the live count was always 0 and the divisor was always missing. The line it
# printed instead ("could not read VmRSS or sessions_live") blamed the node for a number
# this script never asked for at a time when it existed. Sampling both here also gives the
# idle step the one reading no client-side report can carry: the server's own count of the
# sessions it was holding, taken while it held them.
start_rss_monitor() {
  (
    while true; do
      if [ -n "$NODE_PID" ] && kill -0 "$NODE_PID" 2>/dev/null; then
        local rss live
        rss="$(awk '/^VmRSS/{print $2}' "/proc/$NODE_PID/status" 2>/dev/null || true)"
        # --max-time: /metrics is read by a sampler that must keep its cadence, and a
        # node busy enough to be worth sampling is a node whose metrics scrape can take
        # seconds. Without a deadline one slow response stalls the loop and the samples
        # either side of it are simply missing, which is indistinguishable in the log
        # from a step that held no pool. The deadline is 10 s rather than 5 because the
        # loop sleeps 5 s between samples: at 10 s the worst case is still one sample per
        # 15 s, and a scrape that legitimately needs 6 s of a node holding ten thousand
        # sessions is a reading worth having, not a stall.
        #
        # `|| true` is the load-bearing half, and it is not decoration. This script runs
        # under `set -euo pipefail`, so before it existed a scrape that hit the deadline
        # ended the sampler: curl exits 28, pipefail makes that the pipeline's status, and
        # the assignment's non-zero status terminated the subshell. The first scrape slow
        # enough to time out is the scrape taken while the pool is largest, so the
        # instrument died at exactly the moment it existed to measure. That is what the
        # idle step's own evidence shows -- the series runs at a clean 5 s cadence from
        # 20:56:14 and stops dead at 21:00:40, two minutes before the pool finished
        # connecting and three minutes before the hold began, having seen 1,138 live
        # sessions at its last sample. The peak it then reported was the ramp.
        #
        # A scrape that does not answer is recorded as `-`, never as 0. Zero is a claim
        # about the node -- ten thousand sessions and the node holds none -- and it is the
        # false reading this whole block exists to avoid; a dash is a claim about the
        # scrape, which is the true one and the one a reader can act on.
        live="$(curl -fsS --max-time 10 "http://localhost:$NODE_PORT/metrics" 2>/dev/null \
          | awk '/^migo_gateway_sessions_live/{print $2; exit}' || true)"
        if [ -n "$rss" ]; then
          # The timestamp is what makes a sample placeable: these are the only readings
          # taken *during* a step, and without it a peak cannot be attributed to the
          # step it happened in, nor can a gap in the series be seen as a gap.
          echo "${rss} ${live:--} $(date -u +%H:%M:%S)"
        fi
      fi
      sleep 5
    done
  ) >"$RSS_LOG" &
  RSS_MONITOR_PID=$!
}

stop_rss_monitor() {
  if [ -n "$RSS_MONITOR_PID" ]; then
    kill "$RSS_MONITOR_PID" 2>/dev/null || true
    wait "$RSS_MONITOR_PID" 2>/dev/null || true
    RSS_MONITOR_PID=""
  fi
}

report_memory_per_session() {
  local line peak peak_live peak_at live_ceiling samples numeric first_at last_at
  local last_epoch now_epoch age
  # Both halves from one line, and the line is the peak-RSS one. The maxima of two columns
  # are two numbers from two moments, and dividing them is the cross-instant mistake the
  # comment above records one layer out — VmRSS during the run over a sessions_live read
  # after it. Taking them independently is the same mistake one layer in, where it is
  # harder to see because both numbers are real and only their pairing is invented. The
  # highest live count is still printed, separately, because a reader checking the ratio
  # against the pool the client asked for needs to know whether any sample ever saw more.
  # Only samples whose second column is a number take part. A scrape that did not answer
  # is recorded as `-`, and reading that as zero would put a fabricated zero into both the
  # ceiling and the peak selection -- the same cross-instant mistake in a new costume,
  # where the invented number now comes from a failed read rather than from a second moment.
  numeric="$(awk '$2 ~ /^[0-9]+$/' "$RSS_LOG" 2>/dev/null || true)"
  line="$(printf '%s\n' "$numeric" | sort -n | tail -n 1 || true)"
  peak="$(printf '%s\n' "$line" | awk '{print $1}')"
  peak_live="$(printf '%s\n' "$line" | awk '{print $2}')"
  peak_at="$(printf '%s\n' "$line" | awk '{print $3}')"
  live_ceiling="$(printf '%s\n' "$numeric" | awk '{if ($2+0 > m) m = $2+0} END{print m+0}')"
  samples="$(wc -l <"$RSS_LOG" 2>/dev/null || echo 0)"
  if [ -n "$peak" ] && [ "${peak_live:-0}" -gt 0 ] 2>/dev/null; then
    echo "--- memory per session ---"
    echo "  node VmRSS peak ${peak} kB at ${peak_at:-unknown} UTC, the same sample showing" \
      "${peak_live} live sessions = $((peak / peak_live)) kB per session (${samples} samples)"
    echo "  highest live count any sample saw: ${live_ceiling}" \
      "(the ratio above is the peak-RSS sample's own live count, not this one)"
    # What the series covers, because a ceiling is only a ceiling over the window it was
    # taken in. A sampler that dies mid-step leaves a series whose highest reading is the
    # ramp's, and printed beside the client's own `connected 10000/10000` that reads as the
    # node never having held them -- the exact misreading this reading was added to prevent,
    # this time produced by the instrument. The last sample's distance from the moment of
    # this report says whether the series ended with the step or before it.
    first_at="$(head -n 1 "$RSS_LOG" 2>/dev/null | awk '{print $3}')"
    last_at="$(tail -n 1 "$RSS_LOG" 2>/dev/null | awk '{print $3}')"
    echo "  series: ${samples} sample(s) from ${first_at:-unknown} to ${last_at:-unknown} UTC"
    last_epoch="$(date -u -d "$last_at" +%s 2>/dev/null || echo '')"
    now_epoch="$(date -u +%s)"
    if [ -n "$last_epoch" ]; then
      age=$((now_epoch - last_epoch))
      # A run crossing midnight UTC puts the last sample "in the future" of today's date.
      if [ "$age" -lt 0 ]; then age=$((age + 86400)); fi
      if [ "$age" -gt 60 ]; then
        echo "  the series stopped ${age}s before this reading, so it covers only part of the" \
          "step: the peak above is that window's peak and the ceiling is not the step's own"
      fi
    fi
  else
    # Not a footnote: this reading is the step's server-side evidence, and a step that
    # cannot produce it did not hold a pool to measure.
    echo "  (no sample saw both VmRSS and a live session; rss log: ${samples} samples)" >&2
    FAILED_STEPS="$FAILED_STEPS memory-per-session"
    if [ "$FIRST_FAILURE" -eq 0 ]; then FIRST_FAILURE=6; fi
  fi
}

FAILED_STEPS=""
FIRST_FAILURE=0

# The step's report, held to the step's own claim. Zero means the report supports it.
#
# This is the half of the verdict the exit code cannot give. `node main.js` answers "did the run
# exceed a budget?", and both ways a step can lie — a process that drained its event loop and wrote
# nothing, and a run that connected to nothing and therefore exceeded nothing — exit 0. The judge
# reads the report instead, so an empty file, a run with no session in it, a run that ended a fifth
# of the way into its window, and a run that measured nothing all fail the step they belong to.
judge_report() {
  local step="$1" min_connected="$2" report="$3"
  node "$JUDGE" \
    --step "$step" \
    --min-connected "$min_connected" \
    --max-error-rate "$ERROR_BUDGET" \
    "$report"
}

# Runs one loadgen step, prints its report and the node metrics, records the verdict.
run_step() {
  local step="$1"
  shift
  echo ""
  echo "==> step: $step"
  echo "    $*"
  local report="$WORK_DIR/reports/$step.json"
  # How many sessions the step claims. idle-10k is the one whose *name* is a count — ten thousand
  # idle sessions — so it holds its report to all of them; the rest are throughput steps, where the
  # error budget is what governs how many VUs may fail, and one session is the floor beneath which
  # the scenario did not run at all.
  local min_connected=1
  if [ "$step" = "idle-10k" ]; then min_connected="$IDLE_VUS"; fi
  local status
  set +e
  node "$LOADGEN" \
    --api-url "http://localhost:$NODE_PORT" \
    --max-error-rate "$ERROR_BUDGET" \
    --output json \
    "$@" \
    >"$report"
  status=$?
  set -e
  cat "$report"
  print_metrics "$step"
  local verdict=0
  set +e
  judge_report "$step" "$min_connected" "$report"
  verdict=$?
  set -e
  if [ "$status" -ne 0 ]; then
    echo "==> step '$step' FAILED (loadgen exit $status)" >&2
    echo "==> tail of the node's log ($NODE_LOG):" >&2
    tail -n 60 "$NODE_LOG" >&2
    FAILED_STEPS="$FAILED_STEPS $step"
    if [ "$FIRST_FAILURE" -eq 0 ]; then FIRST_FAILURE=$status; fi
  elif [ "$verdict" -ne 0 ]; then
    # Loadgen exited zero, but its report does not describe the step it was asked to run. The
    # node's log goes out with it because the interesting question is what the server saw.
    echo "==> step '$step' FAILED (report does not support a pass; judge exit $verdict)" >&2
    echo "==> tail of the node's log ($NODE_LOG):" >&2
    tail -n 60 "$NODE_LOG" >&2
    FAILED_STEPS="$FAILED_STEPS $step"
    if [ "$FIRST_FAILURE" -eq 0 ]; then FIRST_FAILURE=6; fi
  else
    echo "==> step '$step' passed"
  fi
}

echo "==> Ensuring database $PG_DATABASE exists on $PG_HOST:$PG_PORT"
PGPASSWORD="$PG_PASSWORD" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc \
  "SELECT 1 FROM pg_database WHERE datname='$PG_DATABASE'" | grep -q 1 \
  || PGPASSWORD="$PG_PASSWORD" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
    -c "CREATE DATABASE $PG_DATABASE;" >/dev/null

echo "==> Starting migod on :$NODE_PORT (postgres store, log $NODE_LOG)"
start_node

# Step 1 — ten thousand idle sessions, with the memory-per-session reading.
start_rss_monitor
run_step idle-10k \
  --scenario connect \
  --vus "$IDLE_VUS" \
  --duration "$IDLE_DURATION" \
  --connect-concurrency "$IDLE_CONNECT_CONCURRENCY"
report_memory_per_session
stop_rss_monitor

# Step 2 — a thousand messages per second on the one node (senders x rate).
run_step msg-rate \
  --scenario messaging \
  --vus "$MSG_VUS" \
  --rate "$MSG_RATE" \
  --duration "$MSG_DURATION"

# Step 3 — one conversation at the member ceiling, every member timing delivery.
run_step fanout-256 \
  --scenario fanout \
  --vus "$FANOUT_VUS" \
  --rate "$FANOUT_RATE" \
  --duration "$FANOUT_DURATION"

# Step 4 — five hundred pairs holding calls concurrently (a thousand participants).
run_step calls \
  --scenario calls \
  --vus "$CALLS_VUS" \
  --duration "$CALLS_DURATION"

# Step 5 — a thousand upload lifecycles in flight, closed-loop.
run_step voice-notes \
  --scenario voice-notes \
  --vus "$VOICE_VUS" \
  --rate 0 \
  --duration "$VOICE_DURATION"

# Step 6 — mass sync after an outage. loadgen is a client: it cannot restart the
# server it talks to, so the orchestration lives here. The scenario's senders stream
# through the offline outbox; we wait for its steady-state marker, let
# OUTAGE_KILL_AFTER seconds of traffic cross, SIGKILL the node, restart it on the
# same PostgreSQL store, and let the scenario's settle phase rule: every
# acknowledged message delivered exactly once, every session back to ready. The
# short request timeout keeps in-flight sends inside the outbox's attempt budget.
echo ""
echo "==> step: outage (the node is killed and restarted mid-run)"
OUTAGE_ERR="$WORK_DIR/outage.err"
OUTAGE_REPORT="$WORK_DIR/reports/outage.json"
node "$LOADGEN" \
  --scenario outage \
  --vus "$OUTAGE_VUS" \
  --duration "$OUTAGE_DURATION" \
  --request-timeout-ms 3000 \
  --api-url "http://localhost:$NODE_PORT" \
  --max-error-rate "$ERROR_BUDGET" \
  --output json \
  >"$OUTAGE_REPORT" \
  2>"$OUTAGE_ERR" &
LOADGEN_PID=$!

# Wait for the steady-state marker (the runner logs it once the workloads start),
# bounded: a loadgen that never reaches steady state must not hang the script.
MARKER_SEEN=0
for _ in $(seq 1 600); do
  if grep -q "running for" "$OUTAGE_ERR" 2>/dev/null; then MARKER_SEEN=1; break; fi
  if ! kill -0 "$LOADGEN_PID" 2>/dev/null; then break; fi
  sleep 0.5
done
if [ "$MARKER_SEEN" -ne 1 ]; then
  # Two different failures wear this message, and the exit status is what tells them apart: a
  # loadgen still running is hung, while one that is already gone never reached its workloads at
  # all — which is what actually happens here. It exits 0, because a process whose event loop has
  # drained is a process Node believes finished, so "the run never reached steady state" has been
  # printed over a clean exit for as long as this step has existed. Say which it is.
  OUTAGE_ALIVE=0
  if kill -0 "$LOADGEN_PID" 2>/dev/null; then OUTAGE_ALIVE=1; fi
  echo "==> outage step never reached steady state" >&2
  if [ "$OUTAGE_ALIVE" -eq 1 ]; then
    echo "==> loadgen (pid $LOADGEN_PID) is still running; it never reached its workloads" >&2
    kill "$LOADGEN_PID" 2>/dev/null || true
  fi
  set +e
  wait "$LOADGEN_PID"
  OUTAGE_EARLY_STATUS=$?
  set -e
  echo "==> loadgen exit status $OUTAGE_EARLY_STATUS (before any workload ran)" >&2
  echo "==> report it left behind: $(wc -c <"$OUTAGE_REPORT" 2>/dev/null || echo 0) bytes" >&2
  echo "==> tail of loadgen stderr:" >&2
  tail -n 40 "$OUTAGE_ERR" >&2
  LOADGEN_PID=""
  FAILED_STEPS="$FAILED_STEPS outage"
  if [ "$FIRST_FAILURE" -eq 0 ]; then FIRST_FAILURE=1; fi
else
  sleep "$OUTAGE_KILL_AFTER"
  echo "==> killing the node (SIGKILL, pid $NODE_PID)"
  kill -9 "$NODE_PID" 2>/dev/null || true
  wait "$NODE_PID" 2>/dev/null || true
  sleep 2
  echo "==> restarting the node on the same PostgreSQL store"
  start_node
  set +e
  wait "$LOADGEN_PID"
  OUTAGE_STATUS=$?
  set -e
  LOADGEN_PID=""
  cat "$OUTAGE_REPORT"
  tail -n 5 "$OUTAGE_ERR" || true
  print_metrics outage
  set +e
  judge_report outage 1 "$OUTAGE_REPORT"
  OUTAGE_VERDICT=$?
  set -e
  if [ "$OUTAGE_STATUS" -eq 0 ] && [ "$OUTAGE_VERDICT" -ne 0 ]; then
    echo "==> step 'outage' FAILED (report does not support a pass; judge exit $OUTAGE_VERDICT)" >&2
    OUTAGE_STATUS=6
  fi
  if [ "$OUTAGE_STATUS" -ne 0 ]; then
    echo "==> step 'outage' FAILED (loadgen exit $OUTAGE_STATUS)" >&2
    echo "==> tail of the node's log ($NODE_LOG):" >&2
    tail -n 60 "$NODE_LOG" >&2
    FAILED_STEPS="$FAILED_STEPS outage"
    if [ "$FIRST_FAILURE" -eq 0 ]; then FIRST_FAILURE=$OUTAGE_STATUS; fi
  else
    echo "==> step 'outage' passed"
  fi
fi

# The contract the steps cannot see from inside: the node it hammered through an
# outage must still answer an ordinary health check.
echo ""
echo "==> Checking /health after the run"
if ! curl -fsS "http://localhost:$NODE_PORT/health" >/dev/null; then
  echo "==> node stopped answering /health after the run; tail of its log:" >&2
  tail -n 60 "$NODE_LOG" >&2
  exit 1
fi

if [ -n "$FAILED_STEPS" ]; then
  echo ""
  echo "==> full-scale load FAILED:$FAILED_STEPS" >&2
  exit "$FIRST_FAILURE"
fi

echo ""
echo "==> Full-scale load run passed (all six steps)"
