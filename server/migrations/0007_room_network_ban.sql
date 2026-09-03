-- ---------------------------------------------------------------------------
-- 0007: the network-wide room ban
-- -----------------------------------------------------------------------------

-- One row per account that has lost every chatroom at once. The escalation rule
-- that writes here is the room system's: an account kicked by a *global* admin
-- more than three times is no longer a member anywhere and may not join
-- anywhere — a chatroom's own staff cannot escalate to this, because a room
-- owner who disliked somebody would then only need four rooms to erase them
-- from the network.

-- `until` null means the ban has no expiry; the global admin who lifts it
-- deletes the row, so there is no "unbanned but still rowed" state to read.

create table room_network_ban (
    account_id uuid primary key references account (account_id) on delete cascade,
    reason     text,
    until      timestamptz,
    by_actor   uuid references account (account_id),
    created_at timestamptz not null
);

-- The join check reads one row by account id; that read is the whole index.
create index room_network_ban_account_idx on room_network_ban (account_id);
