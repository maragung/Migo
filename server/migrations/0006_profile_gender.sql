-- 0006_profile_gender -- the gender the account disclosed at registration.
--
-- Applied by `migod migrate`, which embeds this file through SeaORM's migrator.
-- Once applied it must never be edited (docs/04-data-model.md §5): the migrator
-- records only the name ran, not what it contained, so an edit here silently
-- means "applied" on old databases and something else on new ones. Fix a mistake
-- with 0007, not with a rewrite of history.
--
-- Conventions (see 0001_initial.sql header for the long version):
--   * Enumerations are `smallint`, numbered in one place only — here, the
--     comment, mirrored by `migo_store::model::Gender`.
--   * Null is "not disclosed", which is not the same statement as any numbered
--     value: a registration from a client without the field (every client built
--     before this column) writes null, and null is the honest record of that.

-- ---------------------------------------------------------------------------
-- Gender on the profile
-- ---------------------------------------------------------------------------

-- 1 male, 2 female, 3 other; null not disclosed. Stored on the profile rather
-- than the account because it is presentation the user controls, not a
-- credential or a routing fact — the same line `birth_year` sits on, and it is
-- set the same way: once, at registration, by the person it describes.
alter table profile add column gender smallint;
