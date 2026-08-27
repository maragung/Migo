#!/usr/bin/env bash
# Two-node local Migo stack runner.
#
# Brings up two migod instances, each on its own port and its own PostgreSQL
# database, both sharing the Redis cache on localhost:16379, runs the
# TypeScript chat bot from `tools/chatbot` against node 1, and tears
# both nodes down on exit. Designed for a single readable end-to-end
# smoke test, not a long-running service.
#
# Environment variables (all optional, defaults match the repo's CI):
#   MIGOD_BIN        path to the migod binary (default: ../../server/target/debug/migod)
#   PG_HOST          PostgreSQL host (default: localhost)
#   PG_PORT          PostgreSQL port (default: 15432)
#   PG_USER          PostgreSQL user (default: migo)
#   PG_PASSWORD      PostgreSQL password (default: migo)
#   REDIS_URL        Redis URL (default: redis://localhost:16379/0)
#   NODE1_PORT       HTTP/WS port for node 1 (default: 18080)
#   NODE2_PORT       HTTP/WS port for node 2 (default: 18081)
#   BOT_ROUNDS       How many round-trip messages the bot sends (default: 10)
#   KEEP_ALIVE       If set, leave the nodes running after the bot finishes

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

MIGOD_BIN="${MIGOD_BIN:-$REPO_ROOT/server/target/debug/migod}"
PG_HOST="${PG_HOST:-localhost}"
PG_PORT="${PG_PORT:-15432}"
PG_USER="${PG_USER:-migo}"
PG_PASSWORD="${PG_PASSWORD:-migo}"
REDIS_URL="${REDIS_URL:-redis://localhost:16379/0}"
NODE1_PORT="${NODE1_PORT:-18080}"
NODE2_PORT="${NODE2_PORT:-18081}"
BOT_ROUNDS="${BOT_ROUNDS:-10}"
KEEP_ALIVE="${KEEP_ALIVE:-}"

if [ ! -x "$MIGOD_BIN" ]; then
  echo "migod binary not found at $MIGOD_BIN; build it with:" >&2
  echo "  (cd $REPO_ROOT/server && cargo build --bin migod)" >&2
  exit 1
fi

DB1="migo_2node1"
DB2="migo_2node2"

# A small TOML file with every per-test override. We hand it to migod via
# MIGO_CONFIG so the long list of env vars does not crowd the shell here
# and the per-test values live in one place. Generous rate limits and a
# one-token register cost match what a localhost smoke needs; production
# defaults are tuned for the public internet and lock the test out after
# the first request.
CONFIG_FILE="$(mktemp -t migo-2node-XXXXXX.toml)"
trap 'rm -f "$CONFIG_FILE"' EXIT
cat >"$CONFIG_FILE" <<EOF
[rate_limit]
user_burst = 1000
user_refill_per_second = 500
anonymous_burst = 1000
anonymous_refill_per_second = 500
bot_burst = 1000
bot_refill_per_second = 500

[auth]
registration_cost = 1
EOF

echo "==> Ensuring databases $DB1 and $DB2 exist on $PG_HOST:$PG_PORT"
PGPASSWORD="$PG_PASSWORD" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc \
  "SELECT 1 FROM pg_database WHERE datname = '$DB1';" | grep -q 1 \
  || PGPASSWORD="$PG_PASSWORD" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
       -c "CREATE DATABASE $DB1;" >/dev/null
PGPASSWORD="$PG_PASSWORD" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc \
  "SELECT 1 FROM pg_database WHERE datname = '$DB2';" | grep -q 1 \
  || PGPASSWORD="$PG_PASSWORD" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
       -c "CREATE DATABASE $DB2;" >/dev/null

LOG_DIR="$HERE/logs"
mkdir -p "$LOG_DIR"
NODE1_LOG="$LOG_DIR/node-1.log"
NODE2_LOG="$LOG_DIR/node-2.log"

start_node() {
  local id="$1" region="$2" port="$3" db="$4" log="$5"
  echo "==> Starting migod id=$id region=$region port=$port db=$db"
  MIGO_CONFIG="$CONFIG_FILE" \
  MIGO_NODE__ID="$id" \
  MIGO_NODE__REGION="$region" \
  MIGO_NODE__COUNTRY=ID \
  MIGO_NODE__ROLES=api,gateway,room,game \
  MIGO_NODE__ENVIRONMENT=development \
  MIGO_HTTP__BIND="0.0.0.0:$port" \
  MIGO_HTTP__PUBLIC_URL="http://localhost:$port" \
  MIGO_STORE__BACKEND=postgres \
  MIGO_STORE__URL="postgres://$PG_USER:$PG_PASSWORD@$PG_HOST:$PG_PORT/$db" \
  MIGO_CACHE__BACKEND=redis \
  MIGO_CACHE__URL="$REDIS_URL" \
  MIGO_MEDIA__BACKEND=filesystem \
  MIGO_MEDIA__LOCAL_DIR="$HERE/media-$id" \
  MIGO_AUTH__TOKEN_KEY="development-only-insecure-token-key" \
  MIGO_AUTH__ALLOW_REGISTRATION=true \
  RUST_LOG=info \
  "$MIGOD_BIN" >"$log" 2>&1 &
  echo $!
}

NODE1_PID=$(start_node "node-1" "asia" "$NODE1_PORT" "$DB1" "$NODE1_LOG")
NODE2_PID=$(start_node "node-2" "europe" "$NODE2_PORT" "$DB2" "$NODE2_LOG")

cleanup() {
  if [ -z "$KEEP_ALIVE" ]; then
    echo "==> Tearing down nodes"
    kill "$NODE1_PID" "$NODE2_PID" 2>/dev/null || true
    wait "$NODE1_PID" "$NODE2_PID" 2>/dev/null || true
  else
    echo "==> KEEP_ALIVE set, leaving nodes running (node-1 pid=$NODE1_PID, node-2 pid=$NODE2_PID)"
  fi
}
trap cleanup EXIT INT TERM

wait_health() {
  local port="$1" label="$2"
  for _ in $(seq 1 50); do
    if curl -fsS "http://localhost:$port/health" >/dev/null 2>&1; then
      echo "==> $label healthy on :$port"
      return 0
    fi
    sleep 0.2
  done
  echo "$label did not become healthy in time; tail of log:" >&2
  tail -n 40 "$LOG_DIR/$(echo "$label" | tr ' ' _).log" >&2
  return 1
}

wait_health "$NODE1_PORT" "node 1"
wait_health "$NODE2_PORT" "node 2"

BOT_DIR="$REPO_ROOT/tools/chatbot"
if [ ! -d "$BOT_DIR/dist" ] && [ ! -d "$BOT_DIR/node_modules" ]; then
  echo "==> chatbot dependencies not installed; running pnpm install + build"
  (cd "$REPO_ROOT" && pnpm install --frozen-lockfile) >/dev/null
  (cd "$BOT_DIR" && pnpm install --no-frozen-lockfile && pnpm run build) >/dev/null
fi

echo "==> Running chatbot against node 1 (port $NODE1_PORT), $BOT_ROUNDS rounds"
MIGO_API_URL="http://localhost:$NODE1_PORT" \
MIGO_GATEWAY_URL="ws://localhost:$NODE1_PORT/ws" \
BOT_ROUNDS="$BOT_ROUNDS" \
BOT_ALICE_USERNAME="alice_$(date +%s)" \
BOT_BOB_USERNAME="bob_$(date +%s)" \
  node "$BOT_DIR/dist/main.js"

echo "==> Done. Logs: $NODE1_LOG, $NODE2_LOG"
