-- 0016_profile_call_permissions -- the per-kind call policies section 180 asks
-- for, split so that turning video calls off keeps voice calls ringing.
--
-- Applied by `migod migrate`, which embeds this file through SeaORM's migrator.
-- Once applied it must never be edited (docs/04-data-model.md §5): the migrator
-- records only the name ran, not what it contained, so an edit here silently
-- means "applied" on old databases and something else on new ones. Fix a mistake
-- with 0017, not with a rewrite of history.
--
-- Conventions (see 0001_initial.sql header for the long version):
--   * Numeric visibility, the same encoding `who_can_message` uses: 0 nobody,
--     1 friends, 2 everyone. An unknown value reads as the most private option
--     (`Visibility::from_i16`), so a row written by a future build that grows a
--     fourth level is not read as a permissive one by this build.
--   * `not null default 1` — Friends, which is the default section 180 names.
--     A column that is null-able would need a second "unset" meaning that
--     behaves exactly like Friends, and two spellings of one policy is how a
--     gate ends up disagreeing with the settings screen about what it read.
--
-- Why two columns and not three: section 180 also names group calls, and a
-- group call has no per-account ring to gate. `group_join` in migo-calls
-- admits a participant through conversation membership alone (the pairwise
-- questions the 1:1 gate asks have no group counterpart — see the comment on
-- that check), so a "who may call me into a group call" preference would be a
-- stored setting nothing reads. It is left out of the wire rather than offered
-- in a UI that cannot honour it.

-- ---------------------------------------------------------------------------
-- Call permissions on the profile
-- ---------------------------------------------------------------------------

-- Who may ring this account with an audio call. 0 nobody, 1 friends, 2 everyone.
alter table profile
    add column who_can_call_voice smallint not null default 1;

-- Who may ring this account with a video call. Separate from the column above
-- because refusing to be seen is not refusing to be spoken to, which is the one
-- control section 180 lists as mandatory in its own right.
alter table profile
    add column who_can_call_video smallint not null default 1;
