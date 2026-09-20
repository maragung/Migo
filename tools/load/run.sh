#!/usr/bin/env bash
# One-node load test runner.
#
# Brings up a single migod instance on in-memory backends (no PostgreSQL, no
# Redis — a load test's variable of interest is the gateway's session work,
# not the store's), drives `tools/loadgen`'s connect scenario against it, and
# tears the node down on exit. The scenario is fixed and the budget explicit,
# because the contract under test is a gate, not a benchmark: N concurrent
# sessions must open, stay open for the duration, and close cleanly, with the
# error rate under the stated budget — and the node must still answer /health
# afterwards. Wall-clock percentiles are reported for humans to read, never
# asserted, so a slow CI runner fails this script only by actually breaking.
#
# Environment variables (all optional):
#   MIGOD_BIN      path to the migod binary (default: ../../server/target/release/migod)
#   NODE_PORT      HTTP/WS port for the node (default: 18090)
#   LOAD_VUS       concurrent virtual users (default: 50)
#   LOAD_DURATION  how long to hold the sessions (default: 30s)
#   ERROR_BUDGET   loadgen --max-error-rate (default: 0.05)
#   KEEP_ALIVE     if set, leave the node running after the run

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

MIGOD_BIN="${MIGOD_BIN:-$REPO_ROOT/server/target/release/migod}"
NODE_PORT="${NODE_PORT:-18090}"
LOAD_VUS="${LOAD_VUS:-50}"
LOAD_DURATION="${LOAD_DURATION:-30s}"
ERROR_BUDGET="${ERROR_BUDGET:-0.05}"
KEEP_ALIVE="${KEEP_ALIVE:-}"

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

# The same per-test override pattern as tools/2node: one TOML file handed to
# migod via MIGO_CONFIG. Generous rate limits, because a load test that is
# throttled by the anonymous tier is measuring the limiter, not the sessions.
WORK_DIR="$(mktemp -d -t migo-load-XXXXXX)"
CONFIG_FILE="$WORK_DIR/node.toml"
REPORT_FILE="$WORK_DIR/report.json"
NODE_LOG="$WORK_DIR/node.log"
cat >"$CONFIG_FILE" <<EOF
[rate_limit]
user_burst = 5000
user_refill_per_second = 2500
anonymous_burst = 5000
anonymous_refill_per_second = 2500
bot_burst = 5000
bot_refill_per_second = 2500

[auth]
registration_cost = 1

[media]
local_dir = "$WORK_DIR/media"
EOF

echo "==> Starting migod on :$NODE_PORT (memory backends, log $NODE_LOG)"
MIGO_CONFIG="$CONFIG_FILE" \
MIGO_NODE__ID="load-node" \
MIGO_NODE__ROLES=api,gateway,room,game \
MIGO_NODE__ENVIRONMENT=development \
MIGO_HTTP__BIND="127.0.0.1:$NODE_PORT" \
MIGO_HTTP__PUBLIC_URL="http://localhost:$NODE_PORT" \
MIGO_AUTH__TOKEN_KEY="development-only-insecure-token-key" \
MIGO_AUTH__ALLOW_REGISTRATION=true \
RUST_LOG=info \
"$MIGOD_BIN" >"$NODE_LOG" 2>&1 &
NODE_PID=$!

cleanup() {
  if [ -z "$KEEP_ALIVE" ]; then
    echo "==> Tearing down the node"
    kill "$NODE_PID" 2>/dev/null || true
    wait "$NODE_PID" 2>/dev/null || true
    rm -rf "$WORK_DIR"
  else
    echo "==> KEEP_ALIVE set, leaving the node running (pid=$NODE_PID, log=$NODE_LOG)"
  fi
}
trap cleanup EXIT INT TERM

wait_health() {
  for _ in $(seq 1 100); do
    if curl -fsS "http://localhost:$NODE_PORT/health" >/dev/null 2>&1; then
      echo "==> node healthy on :$NODE_PORT"
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

wait_health

echo "==> Load: $LOAD_VUS concurrent sessions for $LOAD_DURATION (error budget $ERROR_BUDGET)"
# The exit code is the first verdict (0 success, 1 nothing connected or fatal,
# 3 error budget exceeded, 4 wire-byte budget exceeded by more than the
# section-171 headroom, 5 the run never finished), so the report is captured for the
# log either way and the status decides the script's own exit — and then the report
# itself is judged against what this gate promises, because an exit code of zero is
# also what a run that never happened returns.
set +e
node "$LOADGEN" \
  --scenario connect \
  --vus "$LOAD_VUS" \
  --duration "$LOAD_DURATION" \
  --api-url "http://localhost:$NODE_PORT" \
  --max-error-rate "$ERROR_BUDGET" \
  --output json \
  >"$REPORT_FILE"
LOAD_STATUS=$?
set -e
cat "$REPORT_FILE"

# The run's own verdict, in full: a nonzero loadgen exit fails the script with
# the same code so CI's failure names the gate that broke. The node's own log
# is dumped too — every rejection the server made (validation, rate limits,
# auth) is explained there, and a load harness that hides the server's error
# message cannot be debugged from the CI log alone.
if [ "$LOAD_STATUS" -ne 0 ]; then
  echo "==> loadgen exited $LOAD_STATUS (1 nothing connected/fatal, 3 error budget exceeded, 4 byte budget exceeded, 5 the run never finished)" >&2
  echo "==> tail of the node's log ($NODE_LOG):" >&2
  tail -n 60 "$NODE_LOG" >&2
  exit "$LOAD_STATUS"
fi

# The exit code above is loadgen's verdict on its budget, and it is not the same question as this
# gate's, which is whether the sessions opened at all. Both directions of that gap have been read as
# a pass: a run whose event loop drains mid-flight writes no report and exits 0, and a run that
# opened nothing has no error over no operations and exits 0 too. So the report is checked against
# what this gate promises — N concurrent sessions opened, held for the duration — and the script
# fails with the judge's own code when it does not describe that run. This is the gate CI runs on
# every push; it was green through a week in which no session it claims to open ever opened.
set +e
node "$JUDGE" \
  --step "$LOAD_VUS-sessions" \
  --min-connected "$LOAD_VUS" \
  --max-error-rate "$ERROR_BUDGET" \
  "$REPORT_FILE"
VERDICT_STATUS=$?
set -e
if [ "$VERDICT_STATUS" -ne 0 ]; then
  echo "==> the run's report does not support a pass (judge exit $VERDICT_STATUS)" >&2
  echo "==> tail of the node's log ($NODE_LOG):" >&2
  tail -n 60 "$NODE_LOG" >&2
  exit "$VERDICT_STATUS"
fi

# The contract the run cannot see from inside: the node it hammered must still
# answer an ordinary health check. If it does not, say why from the node log.
echo "==> Checking /health after the run"
set +e
curl -fsS "http://localhost:$NODE_PORT/health" >/dev/null
HEALTH_STATUS=$?
set -e
if [ "$HEALTH_STATUS" -ne 0 ]; then
  echo "==> node stopped answering /health after the run; tail of its log:" >&2
  tail -n 60 "$NODE_LOG" >&2
  exit 1
fi

echo "==> Load run passed"
