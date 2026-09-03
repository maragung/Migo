-- 0010_passphrase_rename -- the terminology repair: "password" became "passphrase".
--
-- Applied by `migod migrate`, which embeds this file through SeaORM's migrator.
-- Once applied it must never be edited (docs/04-data-model.md §5): the migrator
-- records only that the name ran, not what it contained, so an edit here silently
-- means "applied" on old databases and something else on new ones. Fix a mistake
-- with 0011, not with a rewrite of history.
--
-- The account credential changed *name*, not meaning: the value hashed into
-- `passphrase_hash` is exactly what `password_hash` held, produced by the same
-- Argon2id parameters, and the recovery table keeps its token shape untouched.
-- Renames rather than copy-and-drop, so there is no instant at which an account
-- is credential-less and no window in which a rollback would strand one: the
-- catalog swaps atomically inside the migration transaction.
--
-- The applied 0001/0003 files keep their historical names and columns on disk
-- and in the migrator's ledger — editing an applied migration is forbidden —
-- so this file is the single place the live schema catches up with the code.

alter table account rename column password_hash to passphrase_hash;

alter table password_recovery rename to passphrase_recovery;
alter index password_recovery_account_idx rename to passphrase_recovery_account_idx;
alter index password_recovery_expires_at_idx rename to passphrase_recovery_expires_at_idx;
