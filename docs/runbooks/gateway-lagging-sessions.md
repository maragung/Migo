# Runbook — lagging sessions / backpressure

## Symptoms

`migo_lagging_sessions` rising, `migo_dropped_frames_total{class="coalescable"}` spiking,
`migo_session_queue_depth_bucket` p99 near capacity, users reporting delayed messages.

## Diagnose

1. Is it one node or all? `sum by (node) (migo_lagging_sessions)`.
   _One node_ → local saturation. _All nodes_ → an upstream cause (large room, mesh lag).
2. Check `migo_fanout_duration_seconds{size_bucket}`. A jump in the largest bucket means a
   big room is the source.
3. Check `migo_outbox_lag_seconds`. Rising with lagging sessions means cross-region delivery
   is behind, not the gateway.
4. Check CPU and the TLS/compression share of it. Compression on small frames is a classic
   self-inflicted cause — verify `COMPRESS_MIN_BYTES` was not lowered.

## Mitigate

- Large room: raise its coalescing linger and enable slow mode
  (`migoctl room throttle <room_id> --linger 50ms --slow 5s`).
- Node saturation: drain it (`migoctl node drain <node_id>`); clients get `RECONNECT_HINT`
  with jittered deadlines and move to healthy nodes.
- Widespread: reduce presence fanout globally via the feature flag
  (`features.presence_fanout=aggregated_only`).

## Verify

`migo_lagging_sessions` returns to baseline; no growth in
`migo_dropped_frames_total{class="critical"}` — that counter should always be zero.

## Follow up

If one room caused it, is its edge-shard configuration right for its size
([06-federation.md](../06-federation.md) §5)? If compression was implicated, add the case
to the load suite.
