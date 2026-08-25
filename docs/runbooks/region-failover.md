# Runbook — region failover

## Symptoms

`migo_mesh_peers{state="offline"}` for a whole region, handshake failures from that
region's endpoints, users in that region reconnecting.

## Diagnose

1. Confirm scope: infrastructure provider status, then our own nodes.
2. Is it network-only (nodes healthy, unreachable) or node loss? Network-only is a
   **partition** — do not move room ownership.
3. Identify rooms whose home region is the affected one:
   `migoctl room list --home-region <region>`.

## Mitigate

- Withdraw the region from GeoDNS/anycast; clients fail over on their own (backoff+jitter).
- Rooms homed in the down region stay **read-only** from edge caches while the partition is
  unresolved. Two sequencers for one room corrupts ordering — this is not negotiable.
- Only after the region is confirmed _down_ (not partitioned), transfer ownership:
  `migoctl room transfer-home --from <region> --to <region> --confirm`.
- Cross-region messages queue in the outbox; check `migo_outbox_lag_seconds` stays bounded
  and the queue age limit is not being hit.

## Verify

Affected users are connected elsewhere; `migo_ws_connections` recovers in surviving
regions; no `critical` frame drops; outbox drains after heal without duplicate delivery
(dedup by message id makes replay safe).

## Follow up

Was ownership transfer needed, and did the read-only state show correctly in clients?
Rehearse this in the next game day.
