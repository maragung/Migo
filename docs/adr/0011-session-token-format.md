# ADR-0011 — A fixed-layout MAC'd session token, not a JWT

- **Status:** Accepted · **Date:** 2026-08-19 · **Brief refs:** §46, §47, §119, §125, §162

## Context

Every frame the gateway accepts and every REST call carries an access token, and the
token is read _before_ the caller is authenticated — it is the first untrusted bytes any
request presents. Three properties follow from that position:

1. Verification must be cheap enough to do on every frame, including frames that turn
   out to be junk.
2. The parser is reachable by anyone who can open a socket, so it should be as close to
   "no parser" as a credential can get.
3. Verification must not require a database read, or the store becomes a hard dependency
   of saying "no" to an unauthenticated flood.

The conventional answer is a JWT. The brief does not ask for one; it asks for short-lived
signed access tokens with rotating refresh tokens (§46), device-bound sessions that are
listable and revocable (§47), and re-authentication before sensitive operations (§125).

## Options

| Option                   | Verify cost                     | Parser surface         | Size       | Revocation       |
| ------------------------ | ------------------------------- | ---------------------- | ---------- | ---------------- |
| JWT (HS256)              | JSON parse + base64 + HMAC      | JSON, on every request | ~260 B     | needs a denylist |
| Opaque random token      | one store read per request      | none                   | ~44 B      | immediate        |
| Fixed-layout MAC'd token | base64 + HMAC + 8 slice indexes | none                   | 174 B text | bounded by TTL   |

## Decision

A fixed 130-byte layout, base64url-encoded, tagged with HMAC-SHA-256 over the whole
prefix. Big-endian throughout, every offset a compile-time constant:

```text
  0..1     version           1
  1..17    account id       16
 17..33    device id        16
 33..49    session id       16
 49..57    capabilities      8   bitmask
 57..65    issued at         8   Migo-epoch millis
 65..73    expires at        8
 73..81    authenticated at  8   carried across refreshes
 81..82    region length     1
 82..98    region           16   ASCII, zero padded
 98..130   tag              32   HMAC-SHA-256 over 0..98
```

There is **no algorithm field**. Refresh tokens are 32 opaque random bytes with no
structure; only a keyed tag of them is persisted. The MAC keys for the two are separate,
both derived from one configured root by label (`LABEL_SESSION_TOKEN`,
`LABEL_REFRESH_TOKEN`), so neither can be used to forge the other.

"Custom format" is not "custom crypto" (ADR-0003): the only primitive is HMAC-SHA-256
from an audited crate.

## Consequences

Verification is one MAC and a length check — no allocation, no store read, no JSON. The
token is roughly half a JWT's size on a protocol where it rides every frame. Third-party
tooling cannot read our tokens, which is not a cost: nothing third-party should be
reading our session tokens.

The price is stated plainly rather than assumed away: **a signed token is valid until it
expires.** Revocation takes effect on the next request that reads the session row, so
exposure after a revocation is bounded by `auth.access_ttl_seconds` — fifteen minutes as
shipped. Callers that cannot accept fifteen minutes must use the checked path, and the
module documentation says so at the point of use.

Five refinements came out of building it.

**Dropping the algorithm field is the security decision, not the size saving.** Twenty
years of `alg` has produced `alg: none` acceptance and RS256-verified-as-HS256 key
confusion, and the mitigation is always the same: ignore the header, pin the algorithm.
At that point the field is dead weight that still has to be parsed. A format with no
algorithm field has no algorithm to confuse. The tag covers the version byte for the
same reason — an attacker cannot relabel a v1 token as a future v2 whose fields mean
something else.

**`authenticated_at` had to be a timestamp, not a bit.** `REAUTHENTICATION_REQUIRED`
(1108) guards operations that need proof the human is still present. A freshness _bit_
minted at sign-in still reads fresh fourteen minutes later, which is most of the token's
life. And the timestamp is carried _forward_ across refreshes rather than reset, because
a refresh is not a proof of presence: resetting it would let a stolen refresh token keep
the presence clock fresh forever, which is exactly backwards. This is why the sessions
table has its own `authenticated_at` column instead of reusing `created_at`.

**Refresh reuse is ranked above every other refresh failure.** When a presented refresh
token matches a row that was already rotated, the whole family dies and the checks for
device mismatch and expiry never run. That ordering is deliberate: reuse is the only
signal that distinguishes token theft from an ordinary client bug, and reporting a more
specific error first would spend the signal on a nicer message. Turning exfiltration
from silent persistence into a detected incident is the entire point of rotation.

**The stored refresh tag is keyed, not a bare hash.** A SHA-256 of a 32-byte random
token is already infeasible to invert, so the difference does not matter against a
brute-force attacker. It matters against an attacker who _already has a candidate token_
— from a log, a proxy, a client backup — and a database dump: with a bare hash they can
confirm the guess offline, with a keyed tag they need the token key too. The cost of the
stronger version is one label.

**Region is a fixed 16-byte field, not length-prefixed.** Variable-length means a parser,
and a region label is not the place to introduce one. It is exposed through `peek_region`,
which reads the field _without_ verifying the tag, so a gateway can route a request to the
region that can actually check it before anyone has checked anything — an unverified value
used for routing only, and named to say so.
