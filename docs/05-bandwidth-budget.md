# 05 — Bandwidth budget

"Do more with less bytes" is a measurable claim, so we measure it. These are **budgets**:
a change that exceeds one needs a justification in review, not a shrug.

## 1. Per-event budget (payload + frame header, before TLS)

| Event                            | Budget                           | Notes                                                      |
| -------------------------------- | -------------------------------- | ---------------------------------------------------------- |
| Text message (≤ 120 chars, E2E)  | **≤ 96 B** overhead + ciphertext | 4 B header, ids are raw 16 B, timestamp varint             |
| Message receipt (delivered/read) | ≤ 24 B                           | Cumulative watermark, not per message                      |
| Typing start/stop                | ≤ 12 B                           | Debounced, coalesced, never per keypress (brief §15)       |
| Presence change                  | ≤ 16 B                           | Only on change; aggregated per room (brief §14)            |
| Room member count update         | ≤ 10 B                           | Coalesced, ≥ 5 s apart, delta only                         |
| PING/PONG                        | ≤ 6 B                            | Interval dictated by the server, adaptive on battery saver |
| ACK                              | ≤ 10 B                           | One ACK retires hundreds of frames                         |
| Sync response header             | ≤ 32 B                           | Then only the missing range                                |

## 2. Per-session budget

| Phase                                                       | Budget                   |
| ----------------------------------------------------------- | ------------------------ |
| HELLO + WELCOME + AUTHENTICATE round trip                   | ≤ 512 B total            |
| Cold start (auth, profile, 20 chat previews, unread counts) | ≤ 24 KB                  |
| Idle session, 1 hour, no activity                           | ≤ 8 KB (heartbeats only) |
| Reconnect with resume, nothing missed                       | ≤ 400 B                  |

## 3. Rules that produce these numbers

1. **No polling.** Ever. If you find yourself writing `setInterval` + `fetch`, the answer
   is a subscription (brief §56).
2. **Cursors, not refreshes.** Open a chat → send `have_seq`, receive the delta.
3. **Deltas, not snapshots.** Room state, game state, member lists, leaderboards.
4. **Coalesce the chatty classes** — presence, typing, counters — inside a 15 ms linger.
5. **Batch** small frames into one WS message (one TLS record, one radio wake-up).
6. **Compress only when it wins** (≥ 512 B and ≥ 10 % gain).
7. **Media never goes through the chat path.** Signed URL, direct to object storage,
   thumbnail first, adaptive quality (brief §16).
8. **Paginate everything** with a hard server-side maximum page size.
9. **Lazy-load** history, media, stickers, game assets. Startup loads the minimum needed
   to render the chat list (brief §74).
10. **One radio wake-up is worth ~50 KB of battery.** Prefer one 2 KB batched frame over
    twenty 100 B frames.

## 4. Bandwidth modes (brief §75)

| Mode           | Behaviour                                                                                                                     |
| -------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `Auto`         | Chooses from measured RTT, throughput and (Android) network metering                                                          |
| `Normal`       | Full quality, autoplay thumbnails, animations                                                                                 |
| `LowData`      | No autoplay, smaller images, reduced avatar resolution, presence throttled ×4, longer heartbeat                               |
| `UltraLowData` | Text and receipts only; media on explicit tap; presence for open chat only; heartbeat at the maximum interval; animations off |

The mode is negotiated in `HELLO` so the **server** stops sending what the client will
not render. Client-side filtering saves rendering; server-side filtering saves bytes.

## 5. Measurement

- The gateway exports `migo_frames_total`, `migo_frame_bytes_bucket` and
  `migo_dropped_frames_total`, all labelled by opcode and class. Regressions show up as a
  bytes-per-message shift.
- `tools/loadgen` reports bytes/user/minute per scenario; CI fails the perf job if a
  scenario exceeds its budget by more than 10 %.
- The web client logs a per-session byte counter in development so a feature's cost is
  visible while it is being written, not after launch.
