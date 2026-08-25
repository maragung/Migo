# Runbook — database pool exhaustion

## Symptoms

`migo_db_pool_waiters` > 0 sustained, REST p99 climbing, `1600`-class errors returned,
send-path availability SLO burning.

## Diagnose

1. `migo_db_pool_in_use` at max with waiters → demand or slow queries.
2. `pg_stat_activity`: long-running queries? A migration? A missing index after a deploy?
3. Correlate with the last deploy. New N+1 query patterns are the usual cause.
4. Replica lag — read traffic may have been sent to a lagging replica and retried on the
   primary.

## Mitigate

- Kill the offending long query (`pg_terminate_backend`) if it is a report or migration.
- Shed load: raise rate-limit costs for cold endpoints
  (`migoctl ratelimit set discovery.search 200`).
- Roll back the last deploy if the timing correlates. Migrations are backward compatible
  with the previous release, so rollback is always safe.
- **Do not** simply raise `max_connections` — Postgres degrades with connection count;
  fix the query or add pooling.

## Verify

Waiters back to zero, p99 recovered, no growth in retryable-error counters.

## Follow up

Add the query to the slow-query budget test. If it was an N+1, add an integration test
asserting the query count for that endpoint.
