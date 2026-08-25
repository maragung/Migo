# Runbooks

Each runbook: **symptoms → diagnose → mitigate → verify → follow up**. Written for someone
half-awake who has never seen this failure before.

| Runbook                                                    | Page-worthy                        |
| ---------------------------------------------------------- | ---------------------------------- |
| [gateway-lagging-sessions.md](gateway-lagging-sessions.md) | Yes, above 5 % of connections      |
| [database-pool-exhaustion.md](database-pool-exhaustion.md) | Yes                                |
| [region-failover.md](region-failover.md)                   | Yes                                |
| [redis-loss.md](redis-loss.md)                             | No — degrades gracefully by design |

Rule: a runbook that has not been exercised in a game day is a wish. We rehearse one per
release.
