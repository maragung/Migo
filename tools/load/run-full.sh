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
# budget exceeded, 4 = wire-byte budget exceeded) and bounded in wall-clock by its
# --duration. Latency percentiles (p50/p95/p99) and bytes per user per minute are in
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

if [ ! -x "$MIGOD_BIN" ]; then
  echo "migod binary not found at $MIGOD_BIN; build it with:" >&2
  echo "  (cd $REPO_ROOT/server && cargo build --release --bin migod)" >&2
  exit 1
fi

LOADGEN="$REPO_ROOT/tools/loadgen/dist/main.js"
if [ ! -f "$LOADGEN" ]; then
  echo "loadgen not built at $LOADGEN; build it with:" >&2
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

cleanup() {
  if [ -n "$RSS_MONITOR_PID" ]; then
    kill "$RSS_MONITOR_PID" 2>/dev/null || true
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
  grep -E \
    '^migo_gateway_(sessions_live|frames_dropped_total|frames_in_total|frames_out_total|resume_total)' \
    "$metrics_file" || echo "  (no gateway metric lines)"
  grep -E '^migo_(reconnect_total|media_)' "$metrics_file" || true
}

# Memory per session (the brief's per-session metric): sample the node's VmRSS while
# the idle pool is up, then divide the peak by the sessions the node reports live.
start_rss_monitor() {
  (
    while true; do
      if [ -n "$NODE_PID" ] && kill -0 "$NODE_PID" 2>/dev/null; then
        awk '/^VmRSS/{print $2}' "/proc/$NODE_PID/status" 2>/dev/null || true
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
  local live
  live="$(curl -fsS "http://localhost:$NODE_PORT/metrics" 2>/dev/null \
    | awk '/^migo_gateway_sessions_live/{print $2; exit}')" || live=""
  local peak
  peak="$(sort -n "$RSS_LOG" 2>/dev/null | tail -n 1 || true)"
  if [ -n "$live" ] && [ -n "$peak" ] && [ "$live" -gt 0 ] 2>/dev/null; then
    echo "--- memory per session ---"
    echo "  node VmRSS peak ${peak} kB over ${live} live sessions" \
      "= $((peak / live)) kB per session"
  else
    echo "  (could not read VmRSS or sessions_live; rss log: $(wc -l <"$RSS_LOG" 2>/dev/null || echo 0) samples)" >&2
  fi
}

# "90s" / "2m" -> milliseconds, or 0 when the spelling is not one of those. A zero disables the
# duration half of the report check rather than failing a step over a unit nobody supports.
duration_ms() {
  local spec="${1:-}"
  local value="${spec%[sm]}"
  case "$spec" in
    *s | *m) ;;
    *) printf '0'; return ;;
  esac
  case "$value" in
    '' | *[!0-9]*) printf '0'; return ;;
  esac
  case "$spec" in
    *s) printf '%s' "$((value * 1000))" ;;
    *m) printf '%s' "$((value * 60000))" ;;
  esac
}

FAILED_STEPS=""
FIRST_FAILURE=0

# Runs one loadgen step, prints its report and the node metrics, records the verdict.
#
# The verdict is not the exit status. loadgen leaves with 0 both when a run finished and when it
# ended without finishing at all — the event loop drains mid-`await`, Node exits with its default
# code, nothing reaches stdout — so the step's own report is read back and checked against what the
# step asked for (check-report.mjs). Without that, this function reported six passes for six runs
# that never happened: steps declaring ninety to a hundred and twenty seconds finishing in one to
# seventeen, every report file empty, and not one session ever connected or `/health` ever seeing a
# live gateway session. The elapsed time is printed either way, because a pass in two seconds is the
# shape the old defect took and it should be legible at a glance.
run_step() {
  local step="$1"
  shift
  echo ""
  echo "==> step: $step"
  echo "    $*"

  # Read out of the step's own command line rather than passed separately, so a call site cannot
  # drift from what loadgen was actually asked to do.
  local expect_vus="0" requested_ms="0" previous="" arg
  for arg in "$@"; do
    case "$previous" in
      --vus) expect_vus="$arg" ;;
      --duration) requested_ms="$(duration_ms "$arg")" ;;
    esac
    previous="$arg"
  done

  local report="$WORK_DIR/reports/$step.json"
  local errors="$WORK_DIR/reports/$step.err"
  local started status elapsed
  started="$(date +%s)"
  set +e
  node "$LOADGEN" \
    --api-url "http://localhost:$NODE_PORT" \
    --max-error-rate "$ERROR_BUDGET" \
    --output json \
    "$@" \
    >"$report" \
    2>"$errors"
  status=$?
  set -e
  elapsed=$(( $(date +%s) - started ))

  cat "$report"
  cat "$errors" >&2
  print_metrics "$step"

  local reason=""
  if [ "$status" -ne 0 ]; then
    reason="loadgen exit $status"
  elif ! node "$HERE/check-report.mjs" "$report" \
    --expect-vus "$expect_vus" \
    --requested-ms "$requested_ms"; then
    reason="loadgen exited 0 but the run did not happen"
  fi

  if [ -n "$reason" ]; then
    echo "==> step '$step' FAILED after ${elapsed}s ($reason)" >&2
    echo "==> loadgen's own diagnostics ($errors):" >&2
    tail -n 40 "$errors" >&2
    echo "==> tail of the node's log ($NODE_LOG):" >&2
    tail -n 60 "$NODE_LOG" >&2
    FAILED_STEPS="$FAILED_STEPS $step"
    if [ "$FIRST_FAILURE" -eq 0 ]; then
      if [ "$status" -ne 0 ]; then FIRST_FAILURE=$status; else FIRST_FAILURE=1; fi
    fi
  else
    echo "==> step '$step' passed (${elapsed}s)"
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
  # Say which of the two ways it failed before saying anything else: a loadgen that already exited
  # is a different fault from one still running and stuck, and the old message could not tell them
  # apart. Everything needed to tell is dumped here — the whole of stderr (it is short: a run that
  # never reaches steady state has said almost nothing), whatever part of the report was written,
  # and the node log, which is the only place the server's own view of the run appears. The previous
  # version printed forty lines of one of those three and left the failure undiagnosable.
  echo "==> outage step never reached steady state" >&2
  if kill -0 "$LOADGEN_PID" 2>/dev/null; then
    echo "==> loadgen (pid $LOADGEN_PID) is still running; killing it" >&2
  else
    echo "==> loadgen exited before reaching steady state" >&2
  fi
  kill "$LOADGEN_PID" 2>/dev/null || true
  set +e
  wait "$LOADGEN_PID"
  OUTAGE_STATUS=$?
  set -e
  LOADGEN_PID=""
  echo "==> loadgen exit $OUTAGE_STATUS; its stderr in full ($OUTAGE_ERR):" >&2
  cat "$OUTAGE_ERR" >&2
  echo "==> its report, if any ($OUTAGE_REPORT):" >&2
  if [ -s "$OUTAGE_REPORT" ]; then cat "$OUTAGE_REPORT" >&2; else echo "    (empty)" >&2; fi
  echo "==> tail of the node's log ($NODE_LOG):" >&2
  tail -n 60 "$NODE_LOG" >&2
  FAILED_STEPS="$FAILED_STEPS outage"
  if [ "$FIRST_FAILURE" -eq 0 ]; then
    if [ "$OUTAGE_STATUS" -ne 0 ]; then FIRST_FAILURE=$OUTAGE_STATUS; else FIRST_FAILURE=1; fi
  fi
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
  # The same report check the other five steps get, for the same reason: a zero exit is not a
  # verdict. The duration asked for is split across the settle windows, so only the connected count
  # and the finish are asserted here — a run that reached the marker has already shown it started.
  OUTAGE_REASON=""
  if [ "$OUTAGE_STATUS" -ne 0 ]; then
    OUTAGE_REASON="loadgen exit $OUTAGE_STATUS"
  elif ! node "$HERE/check-report.mjs" "$OUTAGE_REPORT" --expect-vus "$OUTAGE_VUS"; then
    OUTAGE_REASON="loadgen exited 0 but the run did not happen"
  fi
  if [ -n "$OUTAGE_REASON" ]; then
    echo "==> step 'outage' FAILED ($OUTAGE_REASON)" >&2
    echo "==> loadgen's own diagnostics ($OUTAGE_ERR):" >&2
    tail -n 40 "$OUTAGE_ERR" >&2
    echo "==> tail of the node's log ($NODE_LOG):" >&2
    tail -n 60 "$NODE_LOG" >&2
    FAILED_STEPS="$FAILED_STEPS outage"
    if [ "$FIRST_FAILURE" -eq 0 ]; then
      if [ "$OUTAGE_STATUS" -ne 0 ]; then FIRST_FAILURE=$OUTAGE_STATUS; else FIRST_FAILURE=1; fi
    fi
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
