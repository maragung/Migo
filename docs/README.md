# Migo documentation

The product brief lives at [`../migo.md`](../migo.md) (135 sections, the _what_).
These documents are the engineering elaboration — the _how_, and the _why not otherwise_.

| Doc                                                        | Read it when                                        |
| ---------------------------------------------------------- | --------------------------------------------------- |
| [00-vision.md](00-vision.md)                               | You need the product in one page                    |
| [01-architecture.md](01-architecture.md)                   | You are about to add a service, crate or dependency |
| [02-protocol.md](02-protocol.md)                           | You touch anything on the wire                      |
| [03-security-threat-model.md](03-security-threat-model.md) | You touch auth, crypto, permissions, uploads        |
| [04-data-model.md](04-data-model.md)                       | You add a table, index or migration                 |
| [05-bandwidth-budget.md](05-bandwidth-budget.md)           | You add a realtime event or a client fetch          |
| [06-federation.md](06-federation.md)                       | You work on multi-region / server mesh              |
| [07-rooms-and-moderation.md](07-rooms-and-moderation.md)   | You work on rooms, roles, permissions, abuse        |
| [08-economy-games-bots.md](08-economy-games-bots.md)       | You work on currency, XP, gifts, bots, games        |
| [09-observability-ops.md](09-observability-ops.md)         | You deploy, debug production, or add a metric       |
| [10-testing-strategy.md](10-testing-strategy.md)           | Before you call anything "done"                     |
| [11-roadmap.md](11-roadmap.md)                             | You plan work or wonder why a crate is thin         |
| [12-coding-standards.md](12-coding-standards.md)           | You write your first line of Migo code              |
| [adr/](adr/)                                               | You wonder "why on earth did they choose _that_?"   |
| [runbooks/](runbooks/)                                     | Something is on fire                                |

## How these docs are maintained

- `migo.md` is the **product contract**. It is not edited to match the code; the code
  is edited to match it. Where engineering reality forces a deviation, an ADR records it.
- Every ADR is immutable once `Accepted`. Changing a decision means a **new** ADR that
  supersedes the old one. History is a feature.
- Anything describing bytes on the wire must point at
  [`shared/protocol/schema`](../shared/protocol/schema) — the schema is the truth, docs
  are commentary.
