'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { RelationshipKind } from '@migo/sdk';
import type { Id, RelationshipEntry, SuggestedUser } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { Spinner } from './spinner.js';

/**
 * The relationship kinds this panel files people under, as the plain numbers the wire carries.
 *
 * `RelationshipEntry.kind` is a `number` (a newer server may send a value this build has no name
 * for), and comparing a number against an enum member directly trips the workspace's
 * unsafe-enum-comparison rule — so the enum's numeric values are read into number-typed constants
 * once, and the section filters compare number to number. A kind that matches none of them is
 * simply not rendered, never misfiled.
 */
const KIND_FRIEND: number = RelationshipKind.Friend;
const KIND_PENDING_INCOMING: number = RelationshipKind.PendingIncoming;
const KIND_PENDING_OUTGOING: number = RelationshipKind.PendingOutgoing;

/**
 * The Friends tab: the relationship graph, pending requests, suggestions, and people search.
 *
 * The graph is server-owned — every mutation here asks the server and re-reads the result, because a
 * local mirror would drift the moment either party acted from another device. {@link
 * SocialDomain.onFriendEvent} is the signal to re-read: it says the graph moved, not how, so the
 * panel refreshes both the relationships and the suggestions (a new friend changes what is
 * suggested) rather than patching local state.
 */
export function FriendsPanel(): ReactNode {
  const { client } = useMigo();

  const [entries, setEntries] = useState<RelationshipEntry[] | null>(null);
  const [suggestions, setSuggestions] = useState<SuggestedUser[]>([]);
  const [results, setResults] = useState<SuggestedUser[] | null>(null);
  const [query, setQuery] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<ReadonlySet<Id>>(new Set());

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const [relationships, suggested] = await Promise.all([
        client.social.listRelationships(),
        client.social.suggestions(),
      ]);
      setEntries(relationships);
      setSuggestions(suggested);
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // A friend event means the graph changed under us; re-read rather than guess the shape of change.
  useEffect(() => {
    if (!client) {
      return;
    }
    return client.social.onFriendEvent(() => {
      void reload();
    });
  }, [client, reload]);

  // One stable action per button, so `act` can disable a single person's row while it is in flight.
  const request = useCallback(
    (userId: Id): Promise<void> =>
      client ? client.social.friendRequest(userId) : Promise.resolve(),
    [client],
  );
  const respond = useCallback(
    (userId: Id, accept: boolean): Promise<void> =>
      client ? client.social.friendRespond(userId, accept) : Promise.resolve(),
    [client],
  );

  /** Runs one social action for a user, disabling that user's buttons until it settles. */
  async function act(userId: Id, action: () => Promise<void>): Promise<void> {
    setBusy((prev) => new Set(prev).add(userId));
    try {
      await action();
      await reload();
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setBusy((prev) => {
        const next = new Set(prev);
        next.delete(userId);
        return next;
      });
    }
  }

  async function onSearch(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const text = query.trim();
    if (!client || text.length === 0) {
      return;
    }
    try {
      setResults(await client.social.search(text, 20));
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }

  const { friends, incoming, outgoing } = useMemo(() => {
    const list = entries ?? [];
    return {
      friends: list.filter((entry) => entry.kind === KIND_FRIEND),
      incoming: list.filter((entry) => entry.kind === KIND_PENDING_INCOMING),
      outgoing: list.filter((entry) => entry.kind === KIND_PENDING_OUTGOING),
    };
  }, [entries]);

  // Resolve the relationship rows to names once, through the shared profile cache.
  const relatedIds = useMemo(
    () => [...friends, ...incoming, ...outgoing].map((entry) => entry.userId),
    [friends, incoming, outgoing],
  );
  const profiles = useProfiles(relatedIds);

  return (
    <div className="panel">
      <h1 className="panel-title">Friends</h1>

      <form className="panel-search" role="search" onSubmit={(event) => void onSearch(event)}>
        <input
          type="search"
          className="input"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search by username"
          aria-label="Search people by username"
        />
        <button type="submit" className="btn">
          Search
        </button>
      </form>

      {error ? <p className="form-error">{error}</p> : null}

      {entries === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : (
        <>
          <section className="panel-section" aria-label="Friend requests">
            <h2 className="panel-heading">Requests</h2>
            {incoming.length === 0 && outgoing.length === 0 ? (
              <p className="muted">No pending requests.</p>
            ) : (
              <>
                {incoming.map((entry) => (
                  <PersonRow
                    key={entry.userId}
                    id={entry.userId}
                    name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
                    username={profiles.get(entry.userId)?.username}
                    note="wants to be friends"
                    actions={
                      <>
                        <button
                          type="button"
                          className="btn btn-primary"
                          disabled={busy.has(entry.userId)}
                          onClick={() => void act(entry.userId, () => respond(entry.userId, true))}
                        >
                          Accept
                        </button>
                        <button
                          type="button"
                          className="btn btn-ghost"
                          disabled={busy.has(entry.userId)}
                          onClick={() => void act(entry.userId, () => respond(entry.userId, false))}
                        >
                          Decline
                        </button>
                      </>
                    }
                  />
                ))}
                {outgoing.map((entry) => (
                  <PersonRow
                    key={entry.userId}
                    id={entry.userId}
                    name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
                    username={profiles.get(entry.userId)?.username}
                    note="request sent"
                  />
                ))}
              </>
            )}
          </section>

          <section className="panel-section" aria-label="Your friends">
            <h2 className="panel-heading">Friends</h2>
            {friends.length === 0 ? (
              <p className="muted">No friends yet. Add someone from the suggestions below.</p>
            ) : (
              friends.map((entry) => (
                <PersonRow
                  key={entry.userId}
                  id={entry.userId}
                  name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
                  username={profiles.get(entry.userId)?.username}
                />
              ))
            )}
          </section>

          {results !== null ? (
            <section className="panel-section" aria-label="Search results">
              <h2 className="panel-heading">Search results</h2>
              {results.length === 0 ? (
                <p className="muted">No one found for “{query.trim()}”.</p>
              ) : (
                results.map((person) => (
                  <PersonRow
                    key={person.accountId}
                    id={person.accountId}
                    name={person.displayName}
                    username={person.username}
                    note={mutualNote(person)}
                    actions={
                      <button
                        type="button"
                        className="btn btn-primary"
                        disabled={busy.has(person.accountId)}
                        onClick={() => void act(person.accountId, () => request(person.accountId))}
                      >
                        Add friend
                      </button>
                    }
                  />
                ))
              )}
            </section>
          ) : null}

          <section className="panel-section" aria-label="Suggested friends">
            <h2 className="panel-heading">Suggestions</h2>
            {suggestions.length === 0 ? (
              <p className="muted">No suggestions right now.</p>
            ) : (
              suggestions.map((person) => (
                <PersonRow
                  key={person.accountId}
                  id={person.accountId}
                  name={person.displayName}
                  username={person.username}
                  note={mutualNote(person)}
                  actions={
                    <button
                      type="button"
                      className="btn btn-primary"
                      disabled={busy.has(person.accountId)}
                      onClick={() => void act(person.accountId, () => request(person.accountId))}
                    >
                      Add friend
                    </button>
                  }
                />
              ))
            )}
          </section>
        </>
      )}
    </div>
  );
}

/** The mutual-friends line under a suggested person, omitted when the count is zero. */
function mutualNote(person: SuggestedUser): string | undefined {
  return person.mutualFriends > 0 ? `${person.mutualFriends} mutual friends` : undefined;
}

interface PersonRowProps {
  id: Id;
  name: string;
  username?: string;
  note?: string;
  actions?: ReactNode;
}

/** One person in a list: avatar, name, @username, an optional note, and optional actions. */
function PersonRow({ id, name, username, note, actions }: PersonRowProps): ReactNode {
  return (
    <div className="person-row">
      <Avatar name={name} id={id} size={36} />
      <div className="person-main">
        <span className="person-name">{name}</span>
        {username ? <span className="person-sub">@{username}</span> : null}
        {note ? <span className="person-note">{note}</span> : null}
      </div>
      {actions ? <div className="person-actions">{actions}</div> : null}
    </div>
  );
}
