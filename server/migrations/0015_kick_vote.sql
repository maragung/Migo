-- 0015_kick_vote -- the kick vote's tally, shared by every node over the
-- store.
--
-- A kick vote used to live in a Mutex<HashMap> inside the rooms service and
-- another inside the messaging service, one per process, and both of them
-- claimed the same rule: one question at a time per room, one question at a
-- time per group. Over a shared store that claim is a lie the moment there are
-- two nodes — node A's registry has no idea node B opened a vote, so two
-- tallies run against the same room, two factions each reach their own
-- threshold, and the room is kicked out of members nobody agreed to remove.
-- The tally is a fact about the subject it names, exactly like the membership
-- rows it can change, and facts about a subject belong where every node that
-- holds the subject reads the same answer.
--
-- The table is keyed by the subject — a room in the rooms crate, a group
-- conversation in the messaging crate, the two id spaces being random 128-bit
-- values that do not collide. At most one row per subject is the "one question
-- at a time" rule, and the primary key is its whole enforcement. The voices
-- are a child table rather than an array column because a voice has a voter
-- and nothing else, and `(subject_id, voter_id)` as a key makes "every account
-- speaks once" a constraint instead of a hope; the parent row's lock is what
-- makes the count-and-decide of a cast atomic against any other node casting
-- at the same time.
--
-- Neither table carries a foreign key, deliberately. The subject is sometimes
-- a room and sometimes a conversation, and a reference cannot name either
-- without lying about the other; and the row's whole life is a minute
-- (KICK_VOTE_TTL_MS), closed by its own threshold, by its target's departure,
-- or by the lazy expiry the next vote in the same subject performs — long
-- before any tombstone could orphan it. There is no `closed_at` either: a
-- closed tally is a deleted row, because the closing is announced by the
-- service in the moment, on a frame, and a row that lingered to say "I am
-- finished" would only be a vote that finished blocking the next one.
--
-- Editing an applied migration is forbidden (0001's header, docs/04-data-model.md
-- §5): this is a new file, not an edit.

create table kick_vote (
    subject_id  uuid        not null,
    target_id   uuid        not null,
    opened_at   timestamptz not null,
    primary key (subject_id)
);

create table kick_vote_voter (
    subject_id  uuid    not null,
    voter_id    uuid    not null,
    primary key (subject_id, voter_id)
);
