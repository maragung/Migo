#!/usr/bin/env bash
# Infra stack smoke test: start the compose stack and prove it serves.
#
# This is the other half of infra/'s validation. `make infra-check`
# (tools/scripts/infra-audit.py) reads the files and can never catch a stack
# that builds wrong or boots broken; this script starts the stack the way
# infra/README.md tells a person to, asserts on what it actually serves, and
# tears it down again. Until it existed, section 177 of the brief kept the
# infra item out of BUILT for precisely that reason.
#
# What it proves, and why each assertion is the observable half of a claim
# that until now was only readable from the files:
#
#   1. the server container answers /health, /ready and /metrics, and the
#      config document /v1/config reports the node identity the compose file
#      sets — which is how a drifted env-var name shows up: the process would
#      happily serve under its defaults while the compose wiring reached
#      nothing;
#   2. the gateway route /ws exists on the listener (a plain GET is refused
#      with a client error, not a 404). The protocol behaviour behind it is
#      the two-node nightly smoke's job; this is the deployment-surface claim;
#   3. a real account can be registered through the REST surface, and the row
#      lands IN POSTGRES — queried through the postgres container itself. This
#      is the decisive store check: a stack whose server silently ran on the
#      in-memory backend would still answer 201 while the database container
#      sat idle with no tables at all;
#   4. the server has written state through REDIS — token-bucket keys with the
#      m: prefix, scanned through the redis container itself. The rate limiter
#      degrades silently to local buckets when the cache is unreachable, so a
#      green /health proves nothing about Redis; only this does;
#   5. the web container serves the bundle it was built to serve: /healthz,
#      the index page, and one real /_next/static asset from it.
#
# Where the images come from: built from this tree, by the compose file's own
# `--build`, exactly as the README documents. A prebuilt release artifact could
# vouch only for the tagged commit it was built from, never for the tree this
# script stands on — and testing the Dockerfiles with a foreign binary would
# test nothing the files claim. The price is a cold in-container compile of the
# whole workspace, which is why this gate lives in the nightly workflow rather
# than in per-PR CI.
#
# What "healthy" means: `docker compose up --wait` — Postgres reporting
# pg_isready, Redis answering redis-cli ping, and the migod and web images
# passing their own HEALTHCHECK definitions — bounded by --wait-timeout, with a
# script-side poll of /health afterwards so a failure names the URL and the
# last body it saw rather than a compose timeout.
#
# Teardown runs from an EXIT trap on every path, success included, and on the
# failure path only it first dumps `compose ps` and the full container logs:
# when the stack comes up broken, the CI log is all a reader gets.
#
# Ports: the compose stack publishes 8080 and 19992 on whatever host runs it
# (the audit checks those mappings statically; this script demonstrates them by
# fetching both). That is right for a CI runner or a throwaway machine and
# wrong for a host where those ports belong to a live node — the up fails fast
# on the bind, which is the safe direction. Never run this on the production
# host.
#
# Usage: tools/scripts/infra-smoke.sh   (or: make infra-smoke)
# Requires: docker compose v2 with --wait support, curl, python3.

set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$HERE/../.." && pwd)"
cd "$REPO_ROOT"

COMPOSE_FILE="infra/compose/docker-compose.yml"
SERVER_URL="http://localhost:8080"
WEB_URL="http://localhost:19992"
# Health polling bound, in seconds: compose's own --wait has already gated on
# the container healthchecks by the time this runs, so this poll is the belt to
# that braces — it exists to name the failing URL, not to wait out a slow boot.
HEALTH_TIMEOUT=30

note() { echo "==> $*"; }

die() {
  echo "FAIL: $*" >&2
  exit 1
}

# --- teardown ----------------------------------------------------------------
# STACK_UP records whether `up` has created anything, so the EXIT trap neither
# tears down a stack that was never started nor dumps logs that cannot exist.
# The trap is the single exit path: every failure — a `die`, a `set -e` trip,
# a crashed helper — reaches it, and on the failure path only it dumps
# `compose ps` and the full container logs BEFORE removing them, because when
# the stack comes up broken, the CI log is all a reader gets.
STACK_UP=0
TMP_FILES=()
teardown() {
  if [ "$STACK_UP" -eq 1 ]; then
    note "tearing the stack down (containers, network, volumes)"
    docker compose -f "$COMPOSE_FILE" down -v --remove-orphans >/dev/null 2>&1 || true
    STACK_UP=0
  fi
}
on_exit() {
  local code=$?
  rm -f "${TMP_FILES[@]}" 2>/dev/null || true
  if [ "$code" -ne 0 ] && [ "$STACK_UP" -eq 1 ]; then
    echo "==> failure (exit $code): dumping container state and logs before teardown" >&2
    docker compose -f "$COMPOSE_FILE" ps -a >&2 || true
    docker compose -f "$COMPOSE_FILE" logs --no-color --timestamps >&2 || true
  fi
  teardown
  exit "$code"
}
trap on_exit EXIT

# --- helpers -----------------------------------------------------------------
# Poll a URL until it answers 2xx, or fail naming the URL and last body seen.
await_http() {
  local url="$1" what="$2" deadline=$((SECONDS + HEALTH_TIMEOUT)) body=""
  note "waiting for $what at $url (bound: ${HEALTH_TIMEOUT}s)"
  while [ "$SECONDS" -lt "$deadline" ]; do
    if body="$(curl -fsS --max-time 5 "$url" 2>/dev/null)"; then
      echo "     answered: $body"
      return 0
    fi
    sleep 2
  done
  die "$what never answered at $url within ${HEALTH_TIMEOUT}s (last body: '$body')"
}

expect_eq() {
  local what="$1" got="$2" want="$3"
  if [ "$got" != "$want" ]; then
    die "$what: expected '$want', got '$got'"
  fi
  echo "     ok: $what = $want"
}

# --- preflight ---------------------------------------------------------------
note "checking the Docker daemon is reachable"
docker info >/dev/null 2>&1 || die "docker info failed: no reachable daemon (or no permission)"

# --- start the stack the way the README says ---------------------------------
# One command, on purpose: `up --build --wait` is the README's quick start plus
# the wait a machine needs. --wait-timeout bounds the unhealthy case (a
# crash-looping container never turns healthy; without the bound, --wait hangs
# until the job timeout and the log says nothing).
note "building and starting the stack (cold compile inside the image; this is the long part)"
# STACK_UP goes up BEFORE the up: a compose failure past the build stage can
# leave containers half-created, and those must reach the trap's teardown and
# log dump too. Tearing down a stack that was never created is a no-op.
STACK_UP=1
docker compose -f "$COMPOSE_FILE" up --build -d --wait --wait-timeout 300 \
  || die "the stack did not come up healthy within the compose wait bound"

# --- the server surface ------------------------------------------------------
await_http "$SERVER_URL/health" "the server liveness probe"
expect_eq "the readiness probe" \
  "$(curl -fsS "$SERVER_URL/ready")" '{"status":"ready"}'

note "checking /metrics renders the Prometheus exposition"
metrics="$(curl -fsS "$SERVER_URL/metrics")"
echo "$metrics" | grep -q '^# TYPE ' \
  || die "/metrics answered but rendered no metric families: $(echo "$metrics" | head -3)"

note "checking /v1/config reports the node identity the compose file sets"
config="$(curl -fsS "$SERVER_URL/v1/config")"
node_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["node"]["id"])' <<<"$config")"
expect_eq "the node id from the compose environment" "$node_id" "migo-dev-1"

note "checking the gateway route /ws exists on the listener"
ws_code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 10 "$SERVER_URL/ws")"
case "$ws_code" in
  4??)
    [ "$ws_code" != "404" ] \
      || die "GET /ws answered 404: the gateway route is missing from the deployment"
    echo "     ok: /ws refused a non-upgrade GET with $ws_code (the route is there)"
    ;;
  *)
    die "GET /ws without upgrade headers answered $ws_code (expected a client error, not a 404)"
    ;;
esac

# --- a real write through the REST surface, into Postgres ---------------------
note "registering an account through POST /v1/auth/register"
register_body="$(mktemp)"
grant="$(mktemp)"
TMP_FILES+=("$register_body" "$grant")
cat >"$register_body" <<'EOF'
{
  "username": "smoketest",
  "passphrase": "copper-tonight-asterisk-73",
  "device": { "display_name": "infra smoke", "platform": "desktop" }
}
EOF
register_code="$(curl -sS -o "$grant" -w '%{http_code}' --max-time 20 \
  -H 'content-type: application/json' --data-binary @"$register_body" \
  "$SERVER_URL/v1/auth/register")"
expect_eq "the register status" "$register_code" "201"
python3 - "$grant" <<'EOF'
import json, sys
with open(sys.argv[1]) as handle:
    grant = json.load(handle)
if not grant.get("access_token"):
    raise SystemExit("FAIL: register answered 201 but the grant carries no access token")
if grant.get("is_new_account") is not True:
    raise SystemExit("FAIL: register answered 201 but is_new_account is not true")
print(f"     ok: account {grant['account_id']} registered, grant issued")
EOF

note "checking the account row is in Postgres (not a silent in-memory backend)"
# If the server had fallen back to the memory backend, this query would find no
# account table at all: only migod's boot migrations create one.
if ! account_rows="$(docker compose -f "$COMPOSE_FILE" exec -T postgres \
    psql -U migo -d migo -tAc 'SELECT count(*) FROM account' 2>&1)"; then
  die "cannot read the account table in the postgres container: $account_rows"
fi
account_rows="$(tr -d '[:space:]' <<<"$account_rows")"
if [ "${account_rows:-0}" -lt 1 ]; then
  die "the account table in Postgres holds $account_rows rows: the registration \
never reached the database, so the server is not running on the postgres backend"
fi
echo "     ok: Postgres holds $account_rows account row(s)"

note "checking the server has written cache state through Redis"
# The rate limiter degrades silently to local buckets when Redis is unreachable,
# so only keys in Redis itself prove the server is using this one. Scanned with
# a short retry: the bucket state carries a TTL and a slow assert could race it.
redis_keys=""
for _ in 1 2 3 4 5 6 7 8; do
  redis_keys="$(docker compose -f "$COMPOSE_FILE" exec -T redis \
    redis-cli --scan --pattern 'm:*' 2>/dev/null | head -3 || true)"
  [ -n "$redis_keys" ] && break
  sleep 1
done
if [ -z "$redis_keys" ]; then
  die "Redis holds no m:* keys: the server never wrote cache state through it, \
so it is not running on the redis backend"
fi
echo "     ok: Redis holds server-written keys, e.g. $(head -1 <<<"$redis_keys")"

# --- the web bundle -----------------------------------------------------------
await_http "$WEB_URL/healthz" "the web container health probe"
note "checking the web container serves the exported bundle"
index="$(curl -fsS --max-time 20 "$WEB_URL/")"
grep -q '<title>Migo' <<<"$index" \
  || die "the web index page does not look like the Migo client: $(head -c 200 <<<"$index")"
asset="$(grep -o '/_next/static/[^" ]*' <<<"$index" | head -1 || true)"
[ -n "$asset" ] || die "the web index page references no /_next/static asset"
curl -fsS --max-time 20 -o /dev/null "$WEB_URL$asset" \
  || die "the bundle asset $asset is not served"
echo "     ok: index page and $asset are served"

# --- teardown, and proof that it released the ports ---------------------------
teardown
for port_url in "$SERVER_URL/health" "$WEB_URL/healthz"; do
  if curl -fsS --max-time 3 "$port_url" >/dev/null 2>&1; then
    die "$port_url still answers after teardown: the stack did not release its ports"
  fi
done
note "PASS: the stack built, served on both published ports, wrote through Postgres and Redis, and tore down clean"
