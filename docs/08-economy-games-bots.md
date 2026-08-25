# 08 — Economy, games and bots

## 1. Economy principles

- **Double-entry ledger, append-only.** Every movement is two legs summing to zero; a
  balance is a projection of the log ([04-data-model.md](04-data-model.md) §3). No mutable
  balance column, no lost coins, full auditability.
- **Idempotent by construction.** Every transaction carries an idempotency key, so a
  retried gift on a flaky network cannot double-charge.
- **Earned, not farmed.** Never reward raw message count (brief §29). Reward daily
  engagement, achievements, events, game outcomes and peer-recognised contribution — all
  with per-account velocity caps and anomaly detection.
- **No cash-out, no gambling mechanics.** Games award XP, points and cosmetics. Anything
  resembling wagering for real value is out of scope (brief §37, §87).
- **Sinks matter more than faucets.** Cosmetics, gifts, frames, themes and event entry
  keep the currency meaningful; an economy that only issues currency inflates to zero.

## 2. Anti-abuse

Velocity limits per account and per graph neighbourhood, new-account restrictions,
self-dealing detection (A↔B gift loops, shared-device clusters), device and payment
fingerprint clustering, and a manual review queue for the top percentile of earners.
Every automated economy action writes an audit entry.

## 3. XP and progression

XP comes from quality signals with daily caps and diminishing returns per source.
Levels are a curve, not a linear count. Badges are awarded by rule evaluation on an event
stream, so a new badge can be backfilled by replaying events (brief §30–31).

## 4. Games: server-authoritative, always

The client sends **intents**; the server owns state, randomness and outcomes (brief §89–90).

```
client                gateway            game engine (room node)
  │ GAME_ACTION ────────▶ validate ────────▶ apply(state, action)
  │                       (session, room,     ├─ server-side RNG (seeded, logged)
  │                        turn, cooldown)    ├─ rule check
  │                                           └─ state delta
  │ ◀──────── GAME_EVENT { delta } ◀──────────┘
```

- Randomness is server-side and logged, so a disputed outcome is verifiable.
- Only **deltas** go on the wire — never the whole state per tick (brief §39).
- Turn/cooldown/action validity are checked before anything mutates.
- Scores from clients are never trusted; leaderboards are computed from server events.
- Replay detection: every action carries a monotonic per-game action id.

Built-in games start with the cheap, deterministic ones — dice, coin, RPS, guess,
trivia, word — because they exercise the whole engine with almost no state.

## 5. Bots

A bot is a first-class actor with an id, an owner, a token, permissions, quotas and a
status (brief §36).

- **Minimum permissions by default.** A new bot can read commands addressed to it and
  reply. Everything else is explicitly granted by a room owner.
- **No database access, ever** (brief §42). Bots use the Bot API like any other client.
- **Sandboxed execution** for hosted bots: CPU, memory, wall-clock, network egress
  allow-list, and message-rate quotas. A bot that exceeds its quota is throttled, then
  suspended, and its owner is notified.
- **Output discipline** (brief §40): one response per command, cooldowns, batched events,
  cached leaderboards. A bot that spams a room is a bandwidth attack with extra steps.
- Marketplace listing requires a permission review; permission escalation requires
  re-review and re-consent from room owners (brief §86).

## 6. Bot API surface

Long-lived WebSocket (same MWP protocol, bot capability set) for events, plus REST for
cold operations. Rust SDK first, then a documented HTTP/WebSocket contract so any language
can implement it (brief §41). Webhooks are available for stateless bots that cannot hold
a socket.
