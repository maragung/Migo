-- ---------------------------------------------------------------------------
-- 0009: the fixed public-room capacity
-- -----------------------------------------------------------------------------

-- A public room seats thirty-three, always. The rule changed in the rooms
-- service: creation no longer sizes a public room by its creator's friendships
-- or by any capacity the request named — the kind fixes the number, because a
-- public room is the open front of the service and its capacity is a property
-- of the service, not a knob for whoever founded the room.

-- This one statement is the whole repair for rooms made before the rule. It is
-- deliberately unconditional within its kind: rooms created by the earliest
-- deployments carry a default of 500 and a hand-set 5000 alike, and every
-- public room ends at 33 whether the old number was above it or — for a room
-- someone squeezed on purpose — below it. Managed rooms (kind 2) keep the
-- friendship ladder and are not touched, and neither are the private
-- conversations that share the kind column's numbering in spirit only.

-- Idempotent by construction: re-running seats the same rooms at the same 33.

update room set max_members = 33 where kind = 1;
