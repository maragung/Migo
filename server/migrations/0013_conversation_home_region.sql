-- 0013_conversation_home_region -- the home node of a conversation.
--
-- Section 170 gives every room a home node, recorded as the `home_region`
-- label on the room row, because a room's single sequencer lives there. A
-- direct or group conversation needs the same label for a different reason:
-- no sequencer (a private message never needed one, section 170's own split),
-- but a fan-out authority. The tiered fan-out that carries a room's events
-- across nodes keeps its watch table on the home node alone, so a
-- conversation's events need one node that holds that table, and the row is
-- the one fact every node that holds a copy of the conversation can read the
-- same answer from.
--
-- The label is stamped at creation by the node that creates the conversation
-- and never derived again — the same rule the room label follows, read for
-- conversations: a home recomputed from whichever node happens to answer
-- would move the watch table mid-flight. A room's conversation carries the
-- room's own home region, so the room and its chat are homed together.
--
-- Rows that predate this column read as the empty string: no label, no home
-- node, and the conversation's events stay local to the node that published
-- them — exactly the behaviour before the column existed, so an upgrade
-- changes nothing about existing deployments.
--
-- Editing an applied migration is forbidden (0001's header, docs/04-data-model.md
-- §5): this is a new file, not an edit.

alter table conversation
    add column home_region text not null default '';
