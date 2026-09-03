-- 0008_conversation_groups -- what a group conversation is called, and who
-- founded it.
--
-- Two columns, two facts:
--
--   * `title` is the name a group's founders chose. It lives on the conversation
--     row, nullable, because direct conversations never have one and a group may
--     go unnamed until a founder bothers. Rooms are not conversations here — a
--     room's name is the room's own.
--   * `conversation_member.role` gains its meaning. The column existed from
--     0001 but no code path ever wrote anything but the default 0; the wire's
--     ConversationRole now numbers Member as 1 and Founder as 2 (0 is Unknown,
--     as it is in every protocol enum), so the rows written before groups
--     existed are renumbered to Member. Nothing is lost: every one of them was
--     a member of a direct conversation, and still is.
--
-- Editing an applied migration is forbidden (0001's header, docs/04-data-model.md
-- §5): this is a new file, not an edit.

alter table conversation
    add column title text;

update conversation_member
    set role = 1
    where role = 0;
