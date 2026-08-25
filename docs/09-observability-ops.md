# 09 — Observability & operations

You cannot operate what you cannot see, and you cannot debug at 3 a.m. what you did not
instrument at 3 p.m.

## 1. The four questions every service must answer

1. Am I up? — `/healthz` (process alive), `/readyz` (dependencies OK, accepting traffic).
2. Am I serving correctly? — RED: **R**ate, **E**rrors, **D**uration per opcode/route.
3. Am I saturated? — USE: **U**tilisation, **S**aturation, **E**rrors per resource
   (CPU, memory, connections, queue depth, DB pool, outbox lag).
4. What happened to _this_ user's message? — trace id propagated from the client frame
   through fanout to delivery.

## 2. Metrics (Prometheus/OpenTelemetry, brief §63)

Naming: `migo_<subsystem>_<thing>_<unit>`. Labels are bounded — **never** user id, room
id, or anything unbounded; that is how you kill a metrics backend.

| Metric                                      | Type      | Why it exists                        |
| ------------------------------------------- | --------- | ------------------------------------ |
| `migo_ws_connections`                       | gauge     | Capacity planning, drain progress    |
| `migo_ws_handshakes_total{result}`          | counter   | Auth failures, version rejects       |
| `migo_frames_total{opcode,dir}`             | counter   | Traffic mix                          |
| `migo_frame_bytes_bucket{opcode}`           | histogram | Bandwidth budget enforcement         |
| `migo_session_queue_depth_bucket`           | histogram | Backpressure health                  |
| `migo_dropped_frames_total{class}`          | counter   | Drop policy actually firing          |
| `migo_lagging_sessions`                     | gauge     | Slow consumers before they hurt      |
| `migo_fanout_duration_seconds{size_bucket}` | histogram | Large-room regressions               |
| `migo_message_e2e_latency_seconds`          | histogram | The number users actually feel       |
| `migo_outbox_lag_seconds`                   | gauge     | Reliability of cross-region delivery |
| `migo_db_pool_{in_use,waiters}`             | gauge     | The most common outage cause         |
| `migo_ratelimit_rejections_total{bucket}`   | counter   | Abuse and mis-tuned limits           |
| `migo_mesh_peers{state}`                    | gauge     | Federation health                    |

## 3. Logging

Structured JSON in production, pretty in development. Every log line carries
`request_id`, `session_id`, `node_id`, `region` and — when sampled — `trace_id`.

A redaction layer sits between the app and the sink: tokens, keys, ciphertext, message
bodies, emails and full IPs never reach a log, at any level (brief §117). Redaction is
opt-out per field with an explicit annotation, and there is a test asserting a
representative secret does not appear in the output.

Levels mean something: `error` = a human must look, `warn` = it self-healed but is
notable, `info` = state changes and lifecycle, `debug` = development, `trace` = firehose.
Anything logged per message is `trace`.

## 4. Tracing

W3C trace context, sampled at 1 % by default and 100 % for errors. The `TRACED` frame
flag carries the context across the WebSocket boundary, so a client-reported "message was
slow" is a searchable trace, not a guess.

## 5. SLOs

| SLO                                              | Target            |
| ------------------------------------------------ | ----------------- |
| Message delivery (same region, both online), p99 | < 250 ms          |
| Message delivery (cross region), p99             | < 600 ms          |
| Gateway handshake, p99                           | < 400 ms          |
| REST read, p99                                   | < 150 ms          |
| Availability (send-message path), monthly        | 99.9 %            |
| Durability of accepted messages                  | no loss after ACK |

Error budget policy: burn 50 % of a month's budget and feature work stops until
reliability work lands. This is written down so it is a rule, not a negotiation.

## 6. Alerts that page

Only these page a human: send-path availability below SLO, error-budget burn rate > 10×,
DB pool exhaustion, outbox lag > 60 s, lagging sessions > 5 % of connections, mesh peers
below quorum, certificate expiry < 7 days. Everything else is a ticket. An alert that
does not require action within the hour must not wake anyone up.

## 7. Deploys

Rolling, one region at a time, with drain: stop accepting → `RECONNECT_HINT` with
jittered deadlines → finish in-flight → flush outbox → exit. Automatic rollback on error
rate or handshake failure regression. Migrations are always backward compatible with the
previously deployed version, so a rollback never faces a schema it cannot read.

## 8. Runbooks

In [`runbooks/`](runbooks/) — each one has symptoms, a diagnosis path, mitigation, and
follow-up. A runbook that has never been followed during a game day is a wish, not a plan.
