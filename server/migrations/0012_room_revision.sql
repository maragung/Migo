-- ---------------------------------------------------------------------------
-- 0012: the room state revision
-- -----------------------------------------------------------------------------

-- Section 156 asks every delta to carry an epoch or state version, so a client
-- that already holds a room can tell it missed a frame and re-read the snapshot
-- rather than re-fetching on every doubt. The rooms surface had no such number:
-- member events and state events were deltas, but nothing linked them to the
-- roster a client held, and a client that lost its session rendered a stale
-- roster or a stale topic until the next change happened to arrive.

-- One counter per room, advanced by the store on every write a snapshot can
-- observe: a membership movement (join, leave, kick, ban, the grace timeout), a
-- role or ownership change, and any settings write. Mutations no wire surface
-- can see — a mute, a per-member permission override — do not advance it, so
-- the number only moves when a client holding an older one is genuinely behind.
-- Existing rooms start at zero, which is the revision they were created at:
-- the owner's seat is the birth state, and nothing was ever published for it.

alter table room add column revision bigint not null default 0;
