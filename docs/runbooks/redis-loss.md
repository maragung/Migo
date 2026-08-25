# Runbook — Redis loss

## Symptoms

Presence shows "unknown", typing indicators gone, `migo_ratelimit_rejections_total` shape
changes, Redis connection errors in logs. **Chat still works** — that is the design.

## Diagnose

Confirm it is Redis and not a code path assuming Redis is authoritative. Anything in Redis
must be reconstructible ([01-architecture.md](../01-architecture.md) §8); a hard failure
here is a bug worth filing.

## Mitigate

- Rate limiting falls back to conservative in-process buckets automatically — degraded but
  never open. Verify rejections are not spiking for legitimate traffic; if they are, raise
  the local fallback capacity temporarily.
- Presence rebuilds from live sessions as clients reconnect or heartbeat. No manual action.
- If Redis will be down for a while, set `features.typing_indicator=off` to stop clients
  waiting on events that will not arrive.

## Verify

Send-path availability unaffected; presence converges within one heartbeat interval after
recovery.

## Follow up

Every incident here should end with either "degraded exactly as designed" or a filed bug.
There is no third acceptable outcome.
