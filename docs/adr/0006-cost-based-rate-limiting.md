# ADR-0006 — One cost-based rate limiter, not forty counters

- **Status:** Accepted · **Date:** 2026-08-18 · **Brief refs:** §50, §70, §120, §121

## Context

The brief lists a dozen distinct limits (messages, joins, friend requests, gifts, bot
commands, API calls). Implementing each ad hoc produces inconsistent behaviour, untestable
interactions and an unauditable abuse surface.

## Decision

A single token-bucket engine. Every operation declares a **cost** in the protocol IDL;
callers spend from buckets keyed by IP, user, device, room and bot. Trust tier scales
capacity (new accounts get less). Rejections return `RATE_LIMITED` with `retry_after_ms`.
Buckets live in Redis when present and fall back to conservative local buckets when not —
degraded but never open.

## Consequences

One mechanism to test, tune and observe; adding an opcode without a cost fails CI; limits
are visible in one table instead of scattered through handlers. Cost tuning becomes a real
activity, informed by `migo_ratelimit_rejections_total`.

Four refinements came out of building it, each recorded because each is a place where the
decision above is not quite enough on its own.

**Trust tier scales the refill rate as well as the capacity.** Scaling capacity alone —
which is what this ADR originally said — changes how large a burst is tolerated and leaves
the sustained rate identical for everybody. A brand new account would then be limited to a
smaller burst and allowed to send at a trusted account's rate indefinitely, and the
sustained rate is the half that matters for abuse. The same applies to the per-surface
factors.

**The surfaces strangers share do not scale with tier at all.** A room's bucket must not
widen because a trusted member happened to send the message that created it: the next
hundred messages could come from a hundred new accounts, and the room's readers would pay
for a budget granted on somebody else's reputation. An IP is the same case — a residential
NAT and a botnet's exit node are indistinguishable from the server — so both are sized from
the ordinary user's budget whoever is asking. That leaves five of the seven surfaces
scaling with tier.

**The atomic primitive belongs to `migo-cache`, not here.** Taking tokens is a
read-modify-write, and doing one from outside Redis means either `WATCH`/`MULTI` or a
compare-and-set loop. Both fail in the same place: a hot subject. With N concurrent writers
on one bucket — a busy room, a large NAT — a compare-and-set succeeds about one time in N,
so the limiter would start refusing traffic it should have allowed and get _less_ accurate
exactly as load rose. `TokenBucketCache::take_tokens` is one Lua script, one round trip,
and cannot contend with itself. Two further properties fell out of writing it: `now` comes
from the caller rather than from Redis, which keeps the script deterministic and therefore
safe to replicate (ADR-0009), and nothing is written on a refusal, which halves the
limiter's own Redis traffic exactly when refusals are the common case and stops a flood
from repeatedly extending the TTL of the bucket bouncing it.

**"Conservative local buckets" cannot mean "divide everything by four".** The fallback
divides the _rate_, because N nodes each enforcing the configured rate would together
enforce N times it and the limiter would loosen during an outage. It does not divide the
capacity below the cost of the operation being charged: an anonymous endpoint bucket holds
ten tokens by default and `AUTHENTICATE` costs ten, so a quarter of it holds two and a
bucket that cannot afford an operation refuses it every time, forever. Dividing naively
would have turned a Redis outage into a total authentication outage — a worse failure than
the one the fallback exists to survive. The capacity is floored at the cost, which puts the
tightening entirely in the rate, which is where it belonged: what a degraded node needs to
limit is throughput, not the size of one request.

A configuration that resolves _any_ bucket smaller than the most expensive operation
reaching it is rejected at startup rather than at 3 a.m., because such a bucket does not
rate limit — it refuses forever, with a `retry_after_ms` that is a lie.
