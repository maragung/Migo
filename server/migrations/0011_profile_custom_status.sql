-- 0011_profile_custom_status -- the free-text status the RICH_PRESENCE feature
-- bit gates (section 148).
--
-- Applied by `migod migrate`, which embeds this file through SeaORM's migrator.
-- Once applied it must never be edited (docs/04-data-model.md §5): the migrator
-- records only the name ran, not what it contained, so an edit here silently
-- means "applied" on old databases and something else on new ones. Fix a mistake
-- with 0012, not with a rewrite of history.
--
-- Conventions (see 0001_initial.sql header for the long version):
--   * Null is "no status set", which is not the same statement as any string: a
--     client built before the RICH_PRESENCE bit (every client today) never sends
--     the field, and null is the honest record of that.
--   * The status lives on the profile row, not in presence: a presence entry
--     evaporates with the connection cache, and a status somebody typed is a
--     durable fact about their profile. The presence service keeps refusing
--     `PresenceUpdate.custom_status` with FEATURE_DISABLED; this column is the
--     documented home that refusal points at.

-- ---------------------------------------------------------------------------
-- Custom status on the profile
-- ---------------------------------------------------------------------------

-- Free text the owner set, shown wherever the profile is; null not set. An
-- empty string clears it (the wire's optional field cannot express "clear", so
-- the empty string carries that meaning, the same convention as `bio`).
alter table profile add column custom_status text;
