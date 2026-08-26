#!/usr/bin/env bash
# Runs the whole Migo stack for local development: migod plus the web client.
#
# `make dev` calls this. It exists so that "run Migo locally" is one command with no external
# dependencies: migod defaults to in-memory backends, so there is no Postgres, no Redis, and no
# container runtime involved. Data does not survive a restart, which is the correct trade for a
# development loop — use `make infra-up && make dev-pg` when durability matters.
#
#   migod       http://localhost:8080   REST /v1, gateway /ws, probes at the root
#   web client  http://localhost:19991  statically served in production, Next dev server here
#
# Both processes share this script's process group. Ctrl-C, a `kill` of this script, or either child
# exiting on its own tears the whole group down — a half-running stack where the server died an hour
# ago and the browser is still retrying is a worse debugging experience than a stack that is plainly
# down.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

# The documented development placeholder. migod refuses to start with this key when
# MIGO_NODE__ENVIRONMENT is anything but development, so it cannot reach production by accident.
: "${MIGO_AUTH__TOKEN_KEY:=development-only-insecure-token-key}"
: "${MIGO_STORE__BACKEND:=memory}"
: "${MIGO_CACHE__BACKEND:=memory}"
: "${MIGO_NODE__ENVIRONMENT:=development}"
: "${MIGO_HTTP__BIND:=127.0.0.1:8080}"
: "${MIGO_HTTP__CORS_ORIGINS:=http://localhost:19991}"
: "${RUST_LOG:=info,migod=debug}"
export MIGO_AUTH__TOKEN_KEY MIGO_STORE__BACKEND MIGO_CACHE__BACKEND MIGO_NODE__ENVIRONMENT \
       MIGO_HTTP__BIND MIGO_HTTP__CORS_ORIGINS RUST_LOG

for tool in cargo pnpm; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "dev: $tool is not on PATH. Run 'make setup' first." >&2
    exit 1
  fi
done

pids=()

# Kill the whole process group rather than the recorded PIDs: cargo and pnpm each spawn the real
# process as a child, and killing the wrapper would leave the server holding port 8080.
cleanup() {
  trap - EXIT INT TERM
  echo
  echo "dev: shutting down."
  for pid in "${pids[@]:-}"; do
    [[ -n "$pid" ]] && kill -TERM "-$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "dev: building the workspace TypeScript packages so @migo/sdk resolves."
pnpm --filter "./packages/*" build

echo "dev: starting migod on ${MIGO_HTTP__BIND} (store=${MIGO_STORE__BACKEND}, cache=${MIGO_CACHE__BACKEND})."
setsid cargo run --manifest-path server/Cargo.toml -p migod -- serve &
pids+=("$!")

echo "dev: starting the web client on http://localhost:19991."
setsid pnpm --filter @migo/web dev &
pids+=("$!")

echo
echo "dev: server  http://localhost:8080"
echo "dev: client  http://localhost:19991"
echo "dev: Ctrl-C stops both."

# Return as soon as either child exits, so a crashed server does not leave a live client.
wait -n
