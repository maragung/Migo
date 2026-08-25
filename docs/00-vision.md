# 00 — Vision in one page

## The product

Migo is a global real-time communication and community platform: private messaging,
group chat, discoverable **Public Rooms** and strongly governed **Managed Rooms**, a
social layer (friends, profiles, feed), a virtual economy (gifts, coins, XP, badges),
and an extensible **bot + mini-game** system.

It should _feel_ like a 2005 messenger — instant, tiny, works on a bad 3G connection —
while being a 2026 platform underneath.

## The five properties we optimise for, in order

1. **Security & privacy.** Private communication is end-to-end encrypted by default,
   with no user-facing switch. Servers route ciphertext they cannot read.
2. **Reliability.** Bad networks are the normal case, not the edge case. Reconnect,
   resume, dedupe, and never lose a message the user believes was sent.
3. **Bandwidth economy.** Every byte on the wire is justified. Binary framing, deltas,
   cursors, thumbnails, no polling. See [05-bandwidth-budget.md](05-bandwidth-budget.md).
4. **Community governance.** Public spaces need real moderation tooling, not a mute
   button. Roles, permissions, audit logs, appeals, and abuse detection are core, not
   an afterthought.
5. **Global reach.** Multi-region by construction: users attach to their nearest
   gateway, servers form an authenticated mesh, regions fail independently.

## What we deliberately do _not_ do

| Not doing                                          | Why                                                                                            |
| -------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| Client-to-client P2P for normal chat               | Leaks user IP, breaks moderation, NAT hell, no reliability win (brief §4)                      |
| Custom cryptographic primitives                    | We are not a crypto research lab. Audited libraries only                                       |
| JSON on the hot path                               | 3–6× the bytes of our binary frame for the same payload                                        |
| Dozens of microservices on day one                 | A modular monolith with role composition scales far enough, and can be split later (brief §92) |
| Real-money gambling mechanics in games             | Legal and ethical non-starter (brief §37, §87)                                                 |
| Server-side reading of private chat for "features" | Any feature that requires plaintext private messages is the wrong feature                      |
| Currency awarded per message                       | Trivially farmed by bots (brief §29)                                                           |

## How we know we succeeded

- A text message costs **< 100 bytes** on the wire, end to end.
- Cold app open to usable chat list: **< 1.5 s** on a mid-range Android over slow 4G.
- A 50 000-member room does not degrade any other room on the same cluster.
- Losing an entire region degrades affected users to "reconnecting", not "data lost".
- An admin with full database access cannot read a single private message.
