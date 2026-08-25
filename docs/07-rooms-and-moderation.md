# 07 — Rooms, roles and moderation

## 1. Public Room vs Managed Room (brief §19–21)

|            | Public Room                | Managed Room                                              |
| ---------- | -------------------------- | --------------------------------------------------------- |
| Join       | Immediate                  | Immediate, invite, or approval                            |
| Roles      | Owner, Moderator, Member   | Owner, Manager, Admin, Moderator, Helper, Member          |
| Moderation | Basic (mute, kick, report) | Full: bans, warnings, slow mode, filters, appeals         |
| Config     | Minimal                    | Rules, permissions matrix, branding, theme, announcements |
| Audit      | Actions logged             | Full audit log, moderator activity log, exportable        |
| Discovery  | Yes                        | Optional (a Managed Room may still be public)             |

Same underlying entity, different governance profile — one `rooms` table, one permission
engine. Two parallel implementations would drift within a quarter.

## 2. Role and permission model

Roles are **presets**; permissions are the truth. Effective permission:

```
effective = role_default(role) | member.grant  &  ~member.deny  &  room.enabled
```

Deny always wins, and a room-level disable (e.g. media off) cannot be overridden by any
role. Permissions are the capability strings from brief §48, plus per-room extras. Every
handler resolves permissions server-side; the client's copy is a UI hint only.

Role changes are audit-logged with actor, target, before, after and reason. Ownership
transfer requires re-authentication (brief §85).

## 3. Anti-spam / anti-flood

Layers, cheapest first:

1. Cost-based rate limits per user, room, and IP.
2. Slow mode (per-room minimum interval), auto-enabled when message rate spikes.
3. Duplicate/near-duplicate detection (normalised text + SimHash window).
4. Link policy per room: off / allow-list / reputation-checked.
5. New-account friction: reduced buckets and no links for the first N minutes in a room.
6. Trust score from account age, verified contact, report history, moderator actions.
7. Automated action ladder: warn → slow → mute → temp ban → escalate to human review.

Every automated action is reversible, logged, and appealable. Automation that cannot be
appealed becomes a support disaster and an abuse vector.

## 4. Discovery ranking (brief §83)

Member count is deliberately **not** the primary signal — it is trivially inflated. The
score combines active unique speakers, message quality (length distribution, reply
depth), 7-day retention, moderator responsiveness, report rate (negative), spam rate
(negative) and bot-message ratio (negative), with new-room exploration boosted for a
short window so fresh rooms can be found.

## 5. Reporting and review

Report → triage (automated classification + severity) → queue by severity → moderator
action → notify reporter (outcome only, never reporter identity to the subject) → appeal
window. Reports about **private** conversations can only include content the reporter
voluntarily attaches from their own device: the server has no plaintext (brief §123).

## 6. Moderator tooling

Because moderators are volunteers under pressure, the tooling has to be fast: one-click
action ladder, a per-user history panel, message context, bulk actions with a confirmation
threshold, undo, and a visible audit trail. Every action requires a reason code; free
text is optional.

## 7. Room lifecycle

Draft → Active → (Archived | Suspended | Deleted). Archived rooms are read-only and keep
their id so links never 404. Suspension is a moderation state with a reason and an appeal
path. Deletion tombstones the room and retains audit records per policy.
