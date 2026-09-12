#!/usr/bin/env bash
# N-node federated migod harness.
#
# Brings up 2-4 migod nodes, each with its own directory (config, media, log),
# its own PostgreSQL database, and — the point — a real mesh link between them
# formed purely by their `federation.peers` configuration: no programmatic
# admission anywhere, exactly what a deployment gets. On top of the running
# stack it runs the TypeScript sync check (src/sync-check.ts), which proves
# which cross-node paths the link carries and reports the ones it does not.
# Everything is torn down on exit.
#
# Environment variables (all optional):
#   NODES        how many nodes to run, 2-4 (default: 2)
#   BASE         first port of the harness range (default: 18200); node i's
#                mesh listener is BASE+2(i-1) and its HTTP listener is
#                BASE+2(i-1)+1, all bound on 127.0.0.1 only
#   MIGOD_BIN    path to a migod binary (default: the released binary at
#                /home/dev/migo-prod/migod, copied out before running — the
#                harness never executes the original in place, and never
#                builds one: if no binary exists, it stops and says so)
#   PG_HOST, PG_PORT, PG_USER, PG_PASSWORD
#                PostgreSQL the nodes run on (default: localhost:15432, migo/migo)
#   SKIP_SYNC    if set to 1, bring the nodes up (and wait for the link) but
#                do not run the sync check
#   KEEP_ALIVE   if set, leave the nodes running when the script exits
#
# The harness never binds 8080, 18081, 18443, or 19992 — those belong to the
# production node on this host — and refuses any BASE whose range would.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"

NODES="${NODES:-2}"
BASE="${BASE:-18200}"
MIGOD_BIN="${MIGOD_BIN:-/home/dev/migo-prod/migod}"
PG_HOST="${PG_HOST:-localhost}"
PG_PORT="${PG_PORT:-15432}"
PG_USER="${PG_USER:-migo}"
PG_PASSWORD="${PG_PASSWORD:-migo}"
SKIP_SYNC="${SKIP_SYNC:-}"
KEEP_ALIVE="${KEEP_ALIVE:-}"

# Production's ports, never ours (this host runs the live node on them).
FORBIDDEN_PORTS="8080 18081 18443 19992"
# The range the harness owns. Wide enough for four nodes with room to spare.
PORT_RANGE_MIN=18200
PORT_RANGE_MAX=18299

if ! [[ "$NODES" =~ ^[0-9]+$ ]] || [ "$NODES" -lt 2 ] || [ "$NODES" -gt 4 ]; then
  echo "NODES must be 2, 3, or 4 (got: $NODES)" >&2
  exit 1
fi
if ! [[ "$BASE" =~ ^[0-9]+$ ]]; then
  echo "BASE must be a number (got: $BASE)" >&2
  exit 1
fi
LAST_PORT=$((BASE + 2 * NODES - 1))
for port in $(seq "$BASE" "$LAST_PORT"); do
  if [ "$port" -lt "$PORT_RANGE_MIN" ] || [ "$port" -gt "$PORT_RANGE_MAX" ]; then
    echo "port $port is outside the harness range $PORT_RANGE_MIN-$PORT_RANGE_MAX; pick BASE inside it" >&2
    exit 1
  fi
  for forbidden in $FORBIDDEN_PORTS; do
    if [ "$port" -eq "$forbidden" ]; then
      echo "port $port belongs to the production node on this host; the harness never binds it" >&2
      exit 1
    fi
  done
  # A port already taken by anything (production or a leftover run) would turn
  # into a confusing bind failure later; refuse it now, naming the port.
  if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
    echo "port $port is already in use; free it (or raise BASE) and retry" >&2
    exit 1
  fi
done

# --- the binary -----------------------------------------------------------------
#
# Never built here, never run from the production directory. The released
# binary is copied into the run directory and the copy is what runs, so the
# harness cannot disturb anything that lives where the original does.

if [ ! -x "$MIGOD_BIN" ]; then
  echo "no migod binary at $MIGOD_BIN" >&2
  echo "this harness never builds one. Point MIGOD_BIN at a released binary" >&2
  echo "(a GitHub Release 'server_migod' asset works) and retry." >&2
  exit 1
fi

RUN_DIR="$(mktemp -d -t migo-nnode-XXXXXX)"
cp "$MIGOD_BIN" "$RUN_DIR/migod"
chmod +x "$RUN_DIR/migod"

# The configuration-driven admission this harness exercises landed after some
# released binaries were cut; a binary without it rejects `federation.peers`
# (unknown field) and every node would die at boot. Fail fast with the reason
# instead of four identical startup logs. A fixed-string match: the regex dot
# in a plain grep matches any byte and passes on binaries that merely happen
# to hold the two words next to each other.
if ! grep -aqF 'cannot apply the configured mesh peers' "$RUN_DIR/migod"; then
  echo "the binary at $MIGOD_BIN predates configuration-driven peer admission:" >&2
  echo "it has no 'federation.peers' support and will reject this harness's config." >&2
  echo "Use a binary built from a tree that has it (MIGOD_BIN) and retry." >&2
  rm -rf "$RUN_DIR"
  exit 1
fi

# --- node identities --------------------------------------------------------------
#
# Each node's mesh identity is derived from a fixed 32-character signing key:
# the node id is the first 16 bytes of the key read as an Id, and the public
# key the peers' entries carry is the Ed25519 public key of the whole seed.
# Deterministic seeds mean the same nodes come back on every run, which is
# what makes admission idempotent across runs.

mapfile -t NODE_IDS < <(python3 - "$NODES" <<'PY'
import base64
import sys

from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

count = int(sys.argv[1])
alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'
for i in range(1, count + 1):
    seed = f'{i:016d}-nnode-mesh-seed'.encode()
    if len(seed) != 32:
        raise SystemExit(f'seed for node {i} is {len(seed)} bytes, not 32')
    n = int.from_bytes(seed[:16], 'big')
    text = ''.join(alphabet[(n >> (5 * (25 - c))) & 31] for c in range(26))
    public = (
        Ed25519PrivateKey.from_private_bytes(seed)
        .public_key()
        .public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)
    )
    # Three fields per node: number, node id text, public key. The runner
    # reads them into parallel arrays below.
    print(i)
    print(text)
    print(base64.b64encode(public).decode())
PY
)

node_id_of()  { echo "${NODE_IDS[$(( ($1 - 1) * 3 + 1 ))]}"; }
node_key_of() { echo "${NODE_IDS[$(( ($1 - 1) * 3 + 2 ))]}"; }
node_mesh_port() { echo "$((BASE + 2 * ($1 - 1)))"; }
node_http_port() { echo "$((BASE + 2 * ($1 - 1) + 1))"; }
node_seed()      { printf '%016d-nnode-mesh-seed' "$1"; }
node_db()        { echo "migo_nnode$1"; }

# --- databases --------------------------------------------------------------------
#
# Created if missing, never dropped: a re-run converges (peer admission is
# idempotent; every run registers fresh accounts and rooms), and dropping
# databases is a decision the operator makes, not a harness.

for i in $(seq 1 "$NODES"); do
  DB="$(node_db "$i")"
  if ! PGPASSWORD="$PG_PASSWORD" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres -tAc \
    "SELECT 1 FROM pg_database WHERE datname = '$DB';" | grep -q 1; then
    echo "==> Creating database $DB on $PG_HOST:$PG_PORT"
    PGPASSWORD="$PG_PASSWORD" psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d postgres \
      -c "CREATE DATABASE $DB;" >/dev/null
  fi
done

# --- per-node directories and configuration -----------------------------------------

PIDS=""
cleanup() {
  if [ -n "$KEEP_ALIVE" ] && [ -n "$PIDS" ]; then
    echo "==> KEEP_ALIVE set, leaving nodes running (pids: $(echo "$PIDS" | tr '\n' ' '))"
    return
  fi
  if [ -n "$PIDS" ]; then
    echo "==> Tearing down nodes (pids: $(echo "$PIDS" | tr '\n' ' '))"
    # Only the PIDs this script started and recorded — never a pattern match,
    # never a broadcast signal: production migod runs on this host too.
    # shellcheck disable=SC2086
    kill $PIDS 2>/dev/null || true
    # shellcheck disable=SC2086
    wait $PIDS 2>/dev/null || true
  fi
  rm -f "$RUN_DIR/migod"
}
trap cleanup EXIT INT TERM

write_config() {
  local i="$1" config="$2"
  local http_port db seed
  http_port="$(node_http_port "$i")"
  db="$(node_db "$i")"
  seed="$(node_seed "$i")"

  cat >"$config" <<EOF
# One node of the N-node harness. Everything here is per-node: the identity,
# the ports, the database, the media directory, and — the part this harness
# exists to exercise — the federation peers, which are the whole of what it
# takes for these nodes to link.

[node]
id = "nnode-$i"
region = "nnode-$i"
country = "ID"
roles = "api,gateway,room,game"
environment = "development"
signing_key = "$seed"
mesh_bind = "127.0.0.1:$(node_mesh_port "$i")"

[http]
bind = "127.0.0.1:$http_port"
public_url = "http://127.0.0.1:$http_port"

[federation]
enabled = true
EOF

  # Every other node, named completely: node id, public key, mesh address,
  # and the region that node itself runs under.
  local j peer_port
  for j in $(seq 1 "$NODES"); do
    if [ "$j" -eq "$i" ]; then
      continue
    fi
    peer_port="$(node_mesh_port "$j")"
    cat >>"$config" <<EOF

[[federation.peers]]
node_id = "$(node_id_of "$j")"
public_key = "$(node_key_of "$j")"
base_url = "wss://127.0.0.1:$peer_port"
region = "nnode-$j"
EOF
  done

  cat >>"$config" <<EOF

[store]
backend = "postgres"
url = "postgres://$PG_USER:$PG_PASSWORD@$PG_HOST:$PG_PORT/$db"

[cache]
backend = "memory"

[media]
backend = "filesystem"
local_dir = "$RUN_DIR/node-$i/media"

[auth]
token_key = "development-only-insecure-token-key"
allow_registration = true
registration_cost = 1

# A localhost harness has no public-internet abuse to absorb; production's
# defaults would lock the sync check out after its first request.
[rate_limit]
user_burst = 1000
user_refill_per_second = 500
anonymous_burst = 1000
anonymous_refill_per_second = 500
bot_burst = 1000
bot_refill_per_second = 500
EOF
}

start_node() {
  local i="$1"
  local dir="$RUN_DIR/node-$i"
  mkdir -p "$dir/media"
  write_config "$i" "$dir/migod.toml"

  echo "==> Starting node $i: mesh :$(node_mesh_port "$i"), http :$(node_http_port "$i"), db $(node_db "$i"), region nnode-$i"
  MIGO_CONFIG="$dir/migod.toml" \
  RUST_LOG=info \
    "$RUN_DIR/migod" >"$dir/migod.log" 2>&1 &
  PIDS="$PIDS
$!"
}

wait_health() {
  local i="$1" port
  port="$(node_http_port "$i")"
  for _ in $(seq 1 150); do
    if curl -fsS "http://127.0.0.1:$port/health" >/dev/null 2>&1; then
      echo "==> Node $i healthy on :$port"
      return 0
    fi
    sleep 0.2
  done
  echo "node $i did not become healthy in time; tail of its log:" >&2
  tail -n 40 "$RUN_DIR/node-$i/migod.log" >&2
  return 1
}

# One metric's value off a node's /metrics, summed over label variants.
metric_of() {
  local port="$1" name="$2"
  curl -fsS "http://127.0.0.1:$port/metrics" 2>/dev/null \
    | awk -v name="$name" '$1 ~ "^" name "{" || $1 == name { total += $NF } END { printf "%d", total + 0 }'
}

# --- bring the stack up --------------------------------------------------------------

echo "==> Harness run directory: $RUN_DIR"
echo "==> $NODES node(s), ports $BASE-$LAST_PORT, binary $(ls -l "$MIGOD_BIN" | awk '{print $5, $9}')"

for i in $(seq 1 "$NODES"); do
  start_node "$i"
  wait_health "$i"
done

# --- prove the link formed -------------------------------------------------------------

echo "==> Waiting for the mesh: each node should have shaken hands at least once"
LINK_OK=1
for _ in $(seq 1 100); do
  LINK_OK=1
  for i in $(seq 1 "$NODES"); do
    port="$(node_http_port "$i")"
    if [ "$(metric_of "$port" migo_federation_handshakes_total)" -lt 1 ]; then
      LINK_OK=0
    fi
  done
  [ "$LINK_OK" -eq 1 ] && break
  sleep 0.5
done
if [ "$LINK_OK" -ne 1 ]; then
  echo "the mesh did not form: at least one node never completed a handshake" >&2
  for i in $(seq 1 "$NODES"); do
    echo "--- node $i log (last 20 lines) ---" >&2
    tail -n 20 "$RUN_DIR/node-$i/migod.log" >&2
  done
  exit 1
fi

for i in $(seq 1 "$NODES"); do
  port="$(node_http_port "$i")"
  added="$(metric_of "$port" migo_federation_peers_added_total)"
  handshakes="$(metric_of "$port" migo_federation_handshakes_total)"
  note=""
  if [ "$added" -ne $((NODES - 1)) ]; then
    # Admission is idempotent: a reused database already holds the peer rows,
    # and the counter only ticks for fresh admissions.
    note=" (database reused from an earlier run — admission is idempotent, the link below is the proof)"
  fi
  echo "==> Node $i: peers_added_total=$added$note, handshakes_total=$handshakes"
done

# --- the sync check ---------------------------------------------------------------------

if [ "$SKIP_SYNC" = "1" ]; then
  echo "==> SKIP_SYNC=1: nodes are up and linked; sync check skipped"
  echo "==> Nodes are up; logs under $RUN_DIR/node-*/migod.log; press Ctrl-C to tear down"
  wait
  exit 0
fi

# The check runs the TypeScript source directly on Node >= 23 (native type
# stripping); an older Node falls back to the compiled dist/, building it if
# needed. The SDK itself is prebuilt in the workspace.
NODE_MAJOR="$(node -p 'process.versions.node.split(".")[0]')"
if [ ! -d "$HERE/node_modules/@migo/sdk" ]; then
  echo "==> Linking workspace dependencies (pnpm install at the repo root)"
  (cd "$REPO_ROOT" && pnpm install) >/dev/null
fi
if [ "$NODE_MAJOR" -ge 23 ] && [ -f "$HERE/src/sync-check.ts" ]; then
  SYNC_CHECK="$HERE/src/sync-check.ts"
elif [ -f "$HERE/dist/sync-check.js" ]; then
  SYNC_CHECK="$HERE/dist/sync-check.js"
else
  echo "==> Node $NODE_MAJOR cannot run TypeScript directly; building the sync check"
  (cd "$HERE" && pnpm run build) >/dev/null
  SYNC_CHECK="$HERE/dist/sync-check.js"
fi

echo "==> Running the cross-node sync check (nodes 1 and 2)"
NODE1_HTTP="http://127.0.0.1:$(node_http_port 1)" \
NODE2_HTTP="http://127.0.0.1:$(node_http_port 2)" \
PGHOST="$PG_HOST" PGPORT="$PG_PORT" PGUSER="$PG_USER" PGPASSWORD="$PG_PASSWORD" \
DB1="$(node_db 1)" DB2="$(node_db 2)" \
  node "$SYNC_CHECK"

echo "==> Done. Node logs: $RUN_DIR/node-*/migod.log; run directory kept: $RUN_DIR"
