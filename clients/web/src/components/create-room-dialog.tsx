'use client';

/**
 * The Create Room dialog: a room is named, addressed, and opened in one flow.
 *
 * The wire's create call is entry — the reply is a join handle, the creator is the first member
 * and its Owner — so this dialog mirrors the join flow's projection exactly: the reply is noted
 * into the conversation list and the room registry and the thread opens, exactly as a joined
 * room's does. A room is the one thing on the wire with a *permanent address* (the slug), so the
 * slug field is deliberate about what it accepts and what it costs: lowercase letters, digits,
 * and hyphens, suggested from the name but editable, because the name can change and the slug
 * cannot.
 */

import { useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { RoomKind } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { roomInfoOf, useRooms } from '@/lib/migo/rooms-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { joinedRoomSummary } from '@/lib/migo/use-join-room.js';

import { Spinner } from './spinner.js';

/** What a slug may contain: the address has to survive every URL and mention it will ever ride. */
const SLUG_PATTERN = /^[a-z0-9][a-z0-9-]*$/;

/**
 * The slug a name suggests: lowercase, spaces to hyphens, everything else stripped.
 *
 * Pure, so the suggestion is testable; a suggestion is only ever a starting point — the field
 * stays editable, and an empty suggestion leaves the field for the user to fill.
 */
export function slugSuggestion(name: string): string {
  return name
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9\s-]/g, '')
    .replace(/[\s-]+/g, '-');
}

/**
 * The Create Room dialog.
 *
 * @param onOpenConversation Hands the created room's conversation to the shell, which switches to
 *   the chats section and opens the thread — the same flow joining a room takes.
 * @param onClose Closes the dialog (backdrop tap, the close button, or a successful create).
 */
export function CreateRoomDialog({
  onOpenConversation,
  onClose,
}: {
  onOpenConversation: (conversationId: Id) => void;
  onClose: () => void;
}): ReactNode {
  const { client } = useMigo();
  const { noteConversation } = useConversations();
  const { noteRoom } = useRooms();

  const [name, setName] = useState('');
  const [slug, setSlug] = useState('');
  const [slugTouched, setSlugTouched] = useState(false);
  const [kind, setKind] = useState<RoomKind>(RoomKind.Public);
  const [topic, setTopic] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The slug follows the name until the user edits it — the suggestion is the common case and
  // retyping a derived slug is the friction this removes.
  useEffect(() => {
    if (!slugTouched) {
      setSlug(slugSuggestion(name));
    }
  }, [name, slugTouched]);

  function onSlugInput(value: string): void {
    setSlugTouched(true);
    setSlug(value.toLowerCase());
  }

  async function submit(): Promise<void> {
    if (!client || busy) {
      return;
    }
    const trimmedName = name.trim();
    const trimmedSlug = slug.trim();
    if (trimmedName.length === 0) {
      setError('Give the room a name.');
      return;
    }
    if (!SLUG_PATTERN.test(trimmedSlug)) {
      setError('The address can use lowercase letters, numbers, and hyphens.');
      return;
    }
    const trimmedTopic = topic.trim();

    setBusy(true);
    setError(null);
    try {
      // Creation is entry: the reply is the join handle, and the projections the join flow uses
      // are the ones this uses — one wire moment, one way to land in the list and the registry.
      const joined = await client.rooms.create(
        trimmedSlug,
        trimmedName,
        kind,
        trimmedTopic.length > 0 ? trimmedTopic : undefined,
      );
      noteConversation(joinedRoomSummary(joined));
      noteRoom(roomInfoOf(joined));
      onClose();
      onOpenConversation(joined.conversationId);
    } catch (cause) {
      setError(friendlyError(cause));
      setBusy(false);
    }
  }

  const isPublic = kind === RoomKind.Public;

  return (
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="Create a room"
      onClick={onClose}
    >
      <div className="modal" onClick={(event) => event.stopPropagation()}>
        <header className="modal-header">
          <h2>Create a room</h2>
          <button type="button" className="icon-btn" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </header>

        <form
          className="modal-body"
          onSubmit={(event: FormEvent<HTMLFormElement>) => {
            event.preventDefault();
            void submit();
          }}
        >
          <div className="segmented" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={isPublic}
              className={isPublic ? 'active' : ''}
              onClick={() => setKind(RoomKind.Public)}
            >
              Public
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={!isPublic}
              className={!isPublic ? 'active' : ''}
              onClick={() => setKind(RoomKind.Managed)}
            >
              Managed
            </button>
          </div>
          <p className="muted">
            {isPublic ? 'A community room — anyone can join.' : 'A room under server moderation.'}
          </p>

          <label className="field-label">
            Name
            <input
              type="text"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="Late night talks"
              maxLength={80}
              autoFocus
            />
          </label>

          <label className="field-label">
            Address <span className="muted">(permanent — this cannot change)</span>
            <input
              type="text"
              value={slug}
              onChange={(event) => onSlugInput(event.target.value)}
              placeholder="late-night-talks"
              maxLength={40}
              aria-label="Room address"
            />
          </label>

          <label className="field-label">
            Topic <span className="muted">(optional)</span>
            <input
              type="text"
              value={topic}
              onChange={(event) => setTopic(event.target.value)}
              placeholder="What this room is about"
              maxLength={200}
            />
          </label>

          {error ? <p className="form-error">{error}</p> : null}

          <button type="submit" className="btn btn-primary btn-block" disabled={busy}>
            {busy ? <Spinner /> : 'Create room'}
          </button>
        </form>
      </div>
    </div>
  );
}
