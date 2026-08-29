#!/usr/bin/env bash
# Single-node public runner for manual browser testing.
#
# Boots a single migod instance on a public-facing host, then a static web
# server on the web port (19991 by default), with the web client's default
# API URL pointed at the same public host so a browser opened on a different
# machine can reach the surface without touching the Server disclosure. The
# CORS allow-list is set to the same public origin so the browser's
# preflight is accepted.
#
# Required:
#   PUBLIC_HOST    the public hostname or IP the browser will hit (no scheme)
#
# Optional (all have sensible defaults):
#   PUBLIC_API_PORT     REST port (default 18080)
#   PUBLIC_WEB_PORT     static web port (default 19991)
#   WEB_OUT_DIR         where the static export lives (default ../../clients/web/out)
#   MIGOD_BIN           path to the migod binary (default ../../server/target/debug/migod)
#   MIGO_CONFIG         config file path (default /tmp/migo-public.toml)
#   KEEP_ALIVE          if set, leave the node running after the script exits

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

if [ -z "${PUBLIC_HOST:-}" ]; then
  echo "PUBLIC_HOST is required (the hostname or IP the browser will reach)" >&2
  exit 1
fi

PUBLIC_API_PORT="${PUBLIC_API_PORT:-18080}"
PUBLIC_WEB_PORT="${PUBLIC_WEB_PORT:-19991}"
WEB_OUT_DIR="${WEB_OUT_DIR:-$REPO_ROOT/clients/web/out}"
MIGOD_BIN="${MIGOD_BIN:-$REPO_ROOT/server/target/debug/migod}"
MIGO_CONFIG="${MIGO_CONFIG:-/tmp/migo-public.toml}"
PG_HOST="${PG_HOST:-localhost}"
PG_PORT="${PG_PORT:-15432}"
PG_USER="${PG_USER:-migo}"
PG_PASSWORD="${PG_PASSWORD:-migo}"
REDIS_URL="${REDIS_URL:-redis://localhost:16379/0}"
KEEP_ALIVE="${KEEP_ALIVE:-}"

PUBLIC_BASE_URL="http://${PUBLIC_HOST}:${PUBLIC_API_PORT}"
PUBLIC_WEB_URL="http://${PUBLIC_HOST}:${PUBLIC_WEB_PORT}"

if [ ! -x "$MIGOD_BIN" ]; then
  echo "migod binary not found at $MIGOD_BIN; build it with:" >&2
  echo "  (cd $REPO_ROOT/server && cargo build --bin migod)" >&2
  exit 1
fi

if [ ! -d "$WEB_OUT_DIR" ]; then
  echo "web static export not found at $WEB_OUT_DIR; build it with:" >&2
  echo "  cd $REPO_ROOT/clients/web && NEXT_PUBLIC_MIGO_API_URL=$PUBLIC_BASE_URL pnpm run build" >&2
  exit 1
fi

# A minimal config that points at the local Postgres + Redis, exposes the
# REST surface on the public host, and grants the public web origin CORS.
# Migration files are not run here; the developer is expected to have
# already applied them (the brief ships `make db-migrate` for that).
cat >"$MIGO_CONFIG" <<EOF
[http]
bind = "0.0.0.0:${PUBLIC_API_PORT}"
public_url = "${PUBLIC_BASE_URL}"
cors_origins = ["${PUBLIC_WEB_URL}"]
max_body_bytes = 1048576
request_timeout_ms = 15000
EOF

# Flush the redis namespace so the previous run's captcha challenges and
# rate-limit buckets do not leak into a fresh boot.
if command -v redis-cli >/dev/null 2>&1; then
  REDIS_HOST="$(printf '%s' "$REDIS_URL" | sed -E 's#^redis://([^:/]+).*#\1#')"
  REDIS_PORT="$(printf '%s' "$REDIS_URL" | sed -E 's#^redis://[^:/]+:([0-9]+).*#\1#')"
  redis-cli -h "$REDIS_HOST" -p "$REDIS_PORT" -n 0 FLUSHALL >/dev/null || true
fi

echo ">>> migod $PUBLIC_BASE_URL  (CORS allow-list: $PUBLIC_WEB_URL)"
echo ">>> web  $PUBLIC_WEB_URL"
echo
echo "Open $PUBLIC_WEB_URL/register/ in your browser."
echo

cleanup() {
  if [ -z "$KEEP_ALIVE" ]; then
    if [ -n "${MIGOD_PID:-}" ] && kill -0 "$MIGOD_PID" 2>/dev/null; then
      kill "$MIGOD_PID" 2>/dev/null || true
      wait "$MIGOD_PID" 2>/dev/null || true
    fi
    if [ -n "${WEB_PID:-}" ] && kill -0 "$WEB_PID" 2>/dev/null; then
      kill "$WEB_PID" 2>/dev/null || true
      wait "$WEB_PID" 2>/dev/null || true
    fi
  fi
}
trap cleanup EXIT INT TERM

# The migod process picks up dev defaults for store/cache/media from the
# environment; everything else lives in $MIGO_CONFIG. The captcha threshold
# is set to 3 so the gate triggers on the third failed sign-in, which is
# the brief's "after N failed attempts" rule.
MIGOD_DATABASE_URL="postgres://${PG_USER}:${PG_PASSWORD}@${PG_HOST}:${PG_PORT}/migo_public" \
MIGO_NODE__ID=node-public \
MIGO_NODE__REGION=public \
MIGO_NODE__COUNTRY=ZZ \
MIGO_NODE__ROLES=api,gateway,room,game \
MIGO_NODE__ENVIRONMENT=development \
MIGO_CONFIG="$MIGO_CONFIG" \
MIGO_STORE__BACKEND=postgres \
MIGO_STORE__URL="postgres://${PG_USER}:${PG_PASSWORD}@${PG_HOST}:${PG_PORT}/migo_public" \
MIGO_CACHE__BACKEND=redis \
MIGO_CACHE__URL="$REDIS_URL" \
MIGO_MEDIA__BACKEND=filesystem \
MIGO_MEDIA__LOCAL_DIR=/tmp/migo-public-media \
MIGO_AUTH__TOKEN_KEY=development-only-insecure-token-key \
MIGO_AUTH__ALLOW_REGISTRATION=true \
MIGO_AUTH__REGISTRATION_COST=1 \
MIGO_AUTH__CAPTCHA_THRESHOLD=3 \
"$MIGOD_BIN" &
MIGOD_PID=$!

# The static web server is the same one the project ships in clients/web;
# the public port and host are passed in explicitly so the static
# listener binds on every interface.
( cd "$WEB_OUT_DIR" && node "$REPO_ROOT/clients/web/tools/serve.mjs" \
    --port "$PUBLIC_WEB_PORT" \
    --host 0.0.0.0 \
    --base "$PUBLIC_WEB_URL" \
) &
WEB_PID=$!

wait "$MIGOD_PID" &
wait "$WEB_PID" &
wait
