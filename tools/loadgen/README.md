# @migo/loadgen

A load generator that drives many virtual clients through the **real** `@migo/sdk` path — REST
register, gateway handshake, key publication, end-to-end sealed sends — and reports throughput,
latency percentiles, and errors by class. Because every virtual user is a real `MigoClient`, a run
measures the system as it actually behaves under real crypto, not a stubbed happy path.

## Build

The tool is a workspace package; build it (and the SDK it depends on) with the workspace toolchain:

```sh
pnpm --filter @migo/loadgen build
```

## Run

Point it at a running server (see `infra/README.md` for the no-container quick start) and pick a
scenario:

```sh
# 50 users, direct E2E conversations, 10 messages/sec each, for one minute
node tools/loadgen/dist/main.js --scenario messaging --vus 50 --rate 10 --duration 1m

# hold 500 concurrent sessions open for two minutes (connection fan-out)
node tools/loadgen/dist/main.js --scenario connect --vus 500 --duration 2m

# presence fan-out, machine-readable output for a CI gate
node tools/loadgen/dist/main.js --scenario presence --vus 100 --output json > run.json
```

`pnpm --filter @migo/loadgen start -- --help` prints the full option list.

## Scenarios

| Scenario    | What each VU does                                                        | Measures                                    |
| ----------- | ------------------------------------------------------------------------ | ------------------------------------------- |
| `messaging` | Pairs hold a direct E2E conversation; the sender streams sealed messages | Send-to-ack latency, message throughput     |
| `presence`  | Flips presence Online/Away at the target rate                            | Presence-update latency and throughput      |
| `connect`   | Registers and holds a gateway session for the whole duration             | Connection setup latency, sustained fan-out |

Every scenario records a **connect** digest (registration + handshake) during ramp-up, so
connection cost is visible even when the steady-state workload is something else.

## Reading the report

Latency lines carry `p50/p90/p99` (plus min/max), never an average alone — the tail is where
trouble hides. Errors are broken out by class per operation, so `remote:RATE_LIMITED` (the server
pushing back) reads differently from `transport` (the socket dying). When the server asks a client
to back off, the generator waits it out rather than hammering the limiter.

Every run also reports **wire bytes**: the gateway-socket bytes each session sent and received
(counted by the SDK transport at post-compression wire size, frame headers included, surviving
reconnects — the same counters the web client shows in its Diagnostics group). The report shows
bytes per user per minute per scenario, and each scenario carries a budget derived from the
per-event and per-session targets of brief sections 56/171, set generously above measured behavior
so it is a regression gate. REST bootstrap traffic (registration, key publication) rides HTTP and
is not part of these counters.

## Exit codes

`0` success · `1` fatal error (or nothing connected) · `2` bad usage · `3` error-rate over budget ·
`4` wire-byte budget exceeded.

Pass `--max-error-rate 0.01` to make a run **fail** (exit 3) when more than 1% of operations error —
useful as a CI gate. A scenario whose wire bytes exceed its per-user per-minute budget by more than
10 percent (the section-171 headroom) fails with exit 4; the budgets live in
`src/wire-bytes.ts` with their derivations.

## Safety

The generator only ever registers fresh throwaway accounts (`--prefix`, default `loadgen_…` — the
username it builds is `prefix_runTag_index`, underscore-separated because the server's credential
rules admit only letters, digits, dots and underscores); it never reads or writes real user data. The target server must allow registration, which it does in
development. Do not point it at a production deployment.
