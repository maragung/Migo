-- 0014_device_invisible -- the per-device invisibility preference.
--
-- Section 14 puts invisibility on the server: the state is projected to
-- Offline before a frame exists, and no code path publishes Invisible. But
-- the preference itself used to live only in the connection cache's presence
-- entry, which has a TTL and dies with the socket — so a user who chose to
-- hide and then lost connectivity for longer than the entry's lease came
-- back Online through the arriving state, which could only inherit Invisible
-- from the account's *other* live devices. A user with one device, or one
-- whose every device reconnected, was flashed to every watching contact.
--
-- The preference is therefore a durable fact about the device row, exactly
-- like `last_seen_at`: `PRESENCE_SET` stamps it before any fan-out, a
-- connecting or reviving device reads it back, and a disconnect deliberately
-- leaves it alone. False is the honest record for every device registered
-- before this column existed — nobody was invisible then, because nothing
-- could remember that they were.
--
-- Editing an applied migration is forbidden (0001's header, docs/04-data-model.md
-- §5): this is a new file, not an edit.

alter table device
    add column invisible boolean not null default false;
