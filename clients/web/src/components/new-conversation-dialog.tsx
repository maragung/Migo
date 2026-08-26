'use client';

import { useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { ConversationKind } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { openConversation } from '@/lib/migo/use-open-conversation.js';

import { Spinner } from './spinner.js';

/**
 * Starts a new Direct or Group conversation from account identifiers.
 *
 * The SDK exposes no username/directory lookup, so members are entered as account IDs (one per line or
 * comma-separated). On success the new conversation is inserted into the shared list and opened.
 */
export function NewConversationDialog({ onClose }: { onClose: () => void }): ReactNode {
  const { client } = useMigo();
  const { noteConversation } = useConversations();

  const [kind, setKind] = useState<ConversationKind>(ConversationKind.Direct);
  const [membersText, setMembersText] = useState('');
  const [title, setTitle] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const isGroup = kind === ConversationKind.Group;

  async function onSubmit(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    if (!client || busy) {
      return;
    }
    const members = parseMembers(membersText);
    if (members.length === 0) {
      setError('Enter at least one account ID.');
      return;
    }
    if (!isGroup && members.length !== 1) {
      setError('A direct conversation needs exactly one other account ID.');
      return;
    }

    setBusy(true);
    setError(null);
    try {
      const options = isGroup && title.trim() ? { title: title.trim() } : {};
      const summary = await client.startConversation(kind, members, options);
      noteConversation(summary);
      onClose();
      openConversation(summary.conversationId);
    } catch (cause) {
      setError(friendlyError(cause));
      setBusy(false);
    }
  }

  return (
    <div
      className="modal-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label="New conversation"
      onClick={onClose}
    >
      <div className="modal" onClick={(event) => event.stopPropagation()}>
        <header className="modal-header">
          <h2>New conversation</h2>
          <button type="button" className="icon-btn" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </header>

        <form className="modal-body" onSubmit={(event) => void onSubmit(event)}>
          <div className="segmented" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={!isGroup}
              className={!isGroup ? 'active' : ''}
              onClick={() => setKind(ConversationKind.Direct)}
            >
              Direct
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={isGroup}
              className={isGroup ? 'active' : ''}
              onClick={() => setKind(ConversationKind.Group)}
            >
              Group
            </button>
          </div>

          {isGroup ? (
            <label className="field-label">
              Group title <span className="muted">(optional)</span>
              <input
                type="text"
                value={title}
                onChange={(event) => setTitle(event.target.value)}
                placeholder="Weekend plans"
                maxLength={120}
              />
            </label>
          ) : null}

          <label className="field-label">
            {isGroup ? 'Member account IDs' : 'Recipient account ID'}
            <textarea
              value={membersText}
              onChange={(event) => setMembersText(event.target.value)}
              placeholder={isGroup ? 'One account ID per line' : 'Account ID'}
              rows={isGroup ? 4 : 2}
              autoFocus
            />
          </label>
          <p className="hint">
            Enter account IDs directly. Username search is not available yet, so ask contacts for
            their ID.
          </p>

          {error ? <p className="form-error">{error}</p> : null}

          <button type="submit" className="btn btn-primary btn-block" disabled={busy}>
            {busy ? <Spinner /> : 'Start conversation'}
          </button>
        </form>
      </div>
    </div>
  );
}

/** Splits on commas and newlines, trims, and drops blanks and duplicates. */
function parseMembers(raw: string): Id[] {
  const seen = new Set<string>();
  const result: Id[] = [];
  for (const piece of raw.split(/[\n,]/)) {
    const value = piece.trim();
    if (value.length > 0 && !seen.has(value)) {
      seen.add(value);
      result.push(value as Id);
    }
  }
  return result;
}
