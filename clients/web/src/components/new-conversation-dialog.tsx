'use client';

import { useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind, RelationshipKind } from '@migo/sdk';
import type { Id, RelationshipEntry, SuggestedUser } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { openConversation } from '@/lib/migo/use-open-conversation.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';

/** The relationship kind constants, as plain numbers so the filter compares number to number. */
const KIND_FRIEND: number = RelationshipKind.Friend;

/** How long the search input idles before a query goes to the server, in milliseconds. */
export const SEARCH_DEBOUNCE_MS = 300;

/** How many people one search asks the server for. */
const SEARCH_LIMIT = 10;

/**
 * The username search behind the member picker.
 *
 * Pure over the client double, so a test can pin the contract: an empty (or whitespace-only)
 * query asks the server nothing and resolves to nothing, and a real query rides through with its
 * limit. A query is trimmed before it is sent — the wire's prefix match would treat the spaces
 * as significant.
 */
export async function searchPeople(
  client: { social: { search: (query: string, limit?: number) => Promise<SuggestedUser[]> } },
  query: string,
): Promise<SuggestedUser[]> {
  const text = query.trim();
  if (text.length === 0) {
    return [];
  }
  return client.social.search(text, SEARCH_LIMIT);
}

/** One candidate person: avatar, name, @username, and the control that picks them. */
export function PersonPickRow({
  accountId,
  displayName,
  username,
  note,
  picked,
  onPick,
}: {
  accountId: Id;
  displayName: string;
  username?: string;
  note?: string;
  /** True when the person is already selected, so the control says so instead of re-adding. */
  picked?: boolean;
  onPick: (accountId: Id) => void;
}): ReactNode {
  return (
    <div className="person-row">
      <Avatar name={displayName} id={accountId} size={32} />
      <div className="person-main">
        <span className="person-name">{displayName}</span>
        {username ? <span className="person-sub">@{username}</span> : null}
        {note ? <span className="person-note">{note}</span> : null}
      </div>
      <div className="person-actions">
        <button
          type="button"
          className="btn"
          disabled={picked}
          onClick={() => onPick(accountId)}
          aria-label={`Select ${displayName}`}
        >
          {picked ? 'Selected' : 'Select'}
        </button>
      </div>
    </div>
  );
}

/**
 * Starts a new Direct or Group conversation from people, not identifiers.
 *
 * Members come from two sources: the friends quick-pick at the top (the relationship graph is
 * the shortest path to a person), and a debounced username search for everyone else. A Direct
 * conversation takes exactly one person; a Group takes any number, collected as chips that can
 * each be removed before the start. On success the new conversation is inserted into the shared
 * list and opened.
 */
export function NewConversationDialog({ onClose }: { onClose: () => void }): ReactNode {
  const { client } = useMigo();
  const { noteConversation } = useConversations();

  const [kind, setKind] = useState<ConversationKind>(ConversationKind.Direct);
  const [title, setTitle] = useState('');
  const [members, setMembers] = useState<Id[]>([]);
  const [friends, setFriends] = useState<RelationshipEntry[]>([]);
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<SuggestedUser[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const isGroup = kind === ConversationKind.Group;

  // The friends quick-pick loads once; it is the graph the panel already knows.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client.social
      .listRelationships()
      .then((entries) => {
        if (!cancelled) {
          setFriends(entries.filter((entry) => entry.kind === KIND_FRIEND));
        }
      })
      .catch(() => {
        // A failed quick-pick leaves the search as the working path.
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  // The debounced search: 300ms of quiet, then one query — never a request per keystroke.
  useEffect(() => {
    const text = query.trim();
    if (!client || text.length === 0) {
      setResults(null);
      setSearching(false);
      return;
    }
    setSearching(true);
    const timer = setTimeout(() => {
      searchPeople(client, text)
        .then((found) => {
          setResults(found);
          setError(null);
        })
        .catch((cause: unknown) => {
          setError(friendlyError(cause));
        })
        .finally(() => {
          setSearching(false);
        });
    }, SEARCH_DEBOUNCE_MS);
    return () => {
      clearTimeout(timer);
    };
  }, [client, query]);

  // Names for friends and selected members resolve through the shared profile cache.
  const profiles = useProfiles(
    useMemo(
      () => [
        ...friends.map((entry) => entry.userId),
        ...members,
        ...(results ?? []).map((person) => person.accountId),
      ],
      [friends, members, results],
    ),
  );

  function toggleMember(accountId: Id): void {
    setMembers((prev) => {
      if (prev.includes(accountId)) {
        return prev.filter((id) => id !== accountId);
      }
      // A Direct conversation is one person: picking replaces.
      if (!isGroup) {
        return [accountId];
      }
      return [...prev, accountId];
    });
  }

  async function onSubmit(): Promise<void> {
    if (!client || busy) {
      return;
    }
    if (members.length === 0) {
      setError('Pick at least one person.');
      return;
    }
    if (!isGroup && members.length !== 1) {
      setError('A direct conversation needs exactly one person.');
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

        <form
          className="modal-body"
          onSubmit={(event) => {
            event.preventDefault();
            void onSubmit();
          }}
        >
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

          {members.length > 0 ? (
            <div className="member-chips" aria-label="Selected members">
              {members.map((id) => (
                <span key={id} className="member-chip">
                  {profiles.get(id)?.displayName ?? 'Someone'}
                  <button
                    type="button"
                    className="chip-remove"
                    onClick={() => toggleMember(id)}
                    aria-label={`Remove ${profiles.get(id)?.displayName ?? 'member'}`}
                  >
                    ✕
                  </button>
                </span>
              ))}
            </div>
          ) : null}

          <label className="field-label">
            Search by username
            <input
              type="search"
              className="input"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="ada"
              autoFocus
              aria-label="Search people by username"
            />
          </label>
          {searching ? (
            <p className="hint">
              <Spinner /> Searching…
            </p>
          ) : null}

          {friends.length > 0 ? (
            <div className="panel-section" aria-label="Friends">
              <h3 className="panel-heading">Friends</h3>
              {friends.map((entry) => (
                <PersonPickRow
                  key={entry.userId}
                  accountId={entry.userId}
                  displayName={profiles.get(entry.userId)?.displayName ?? 'Someone'}
                  username={profiles.get(entry.userId)?.username}
                  picked={members.includes(entry.userId)}
                  onPick={toggleMember}
                />
              ))}
            </div>
          ) : null}

          {results !== null ? (
            <div className="panel-section" aria-label="Search results">
              <h3 className="panel-heading">Search results</h3>
              {results.length === 0 ? (
                <p className="muted">No one found for “{query.trim()}”.</p>
              ) : (
                results.map((person) => (
                  <PersonPickRow
                    key={person.accountId}
                    accountId={person.accountId}
                    displayName={person.displayName}
                    username={person.username}
                    note={
                      person.mutualFriends > 0
                        ? `${person.mutualFriends} mutual friends`
                        : undefined
                    }
                    picked={members.includes(person.accountId)}
                    onPick={toggleMember}
                  />
                ))
              )}
            </div>
          ) : null}

          {error ? <p className="form-error">{error}</p> : null}

          <button type="submit" className="btn btn-primary btn-block" disabled={busy}>
            {busy ? <Spinner /> : 'Start conversation'}
          </button>
        </form>
      </div>
    </div>
  );
}
