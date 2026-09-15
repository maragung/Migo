'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { NotificationKind, RelationshipKind } from '@migo/sdk';
import type { Id, InboxItem } from '@migo/sdk';

import { debounce } from '@/lib/debounce.js';
import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import { Spinner } from './spinner.js';

/** The inbox page the panel asks for; the server owns the ceiling it clamps this to. */
const PAGE_SIZE = 50;

/**
 * The kind the wire uses for a friend request, as the inbox row carries it.
 *
 * The server writes the {@link NotificationKind} enum's number into the row's kind string ("4"),
 * so the comparison is against that — and the same fact is why a request and its acceptance are
 * indistinguishable from the row alone: the server has one kind for both, and the client must ask
 * the graph which it is before it may draw an answer button.
 */
const FRIEND_REQUEST_KIND = String(NotificationKind.FriendRequest);

/** The relationship kind that says a request is waiting on this account, as a plain number. */
const KIND_PENDING_INCOMING: number = RelationshipKind.PendingIncoming;

/** How long a friend-event re-read waits for the events to stop arriving (see the debounce). */
const FRIEND_EVENT_DEBOUNCE_MS = 300;

/**
 * The Notifications tab: the durable inbox and its read state.
 *
 * The live {@link NotificationsDomain.onNotification} stream is droppable by design, so this panel
 * treats it only as a *hint* to re-read the inbox — the rows are the source of truth, and they
 * survive the recipient being offline. An item carries no message content by construction (the
 * server has no plaintext to put there); rendering is kind, actor, and time, and the actor's display
 * name is resolved through the shared profile cache like everywhere else in the app.
 *
 * "Mark all read" acknowledges through the newest rendered item's timestamp — one watermark call
 * that clears everything at or before it, rather than one call per row, so a notification landing
 * mid-flight is left for the next ack instead of being raced.
 *
 * A friend request row is the one kind the wire lets this panel answer: Accept and Decline call
 * the social domain's one respond method, then re-read the graph (for the row's own honesty — a
 * request already answered must stop offering its buttons) and the inbox. Whether the buttons may
 * be drawn at all is a graph question, asked the way the server's own notice design asks the
 * client to ask it: the same kind covers a request and its acceptance, and only the standing
 * (pending-incoming, or not) tells them apart. Every other kind stays inert — the wire carries no
 * action for them here.
 */
export function NotificationsPanel(): ReactNode {
  const { client } = useMigo();

  const [items, setItems] = useState<InboxItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The actors whose friend requests are genuinely pending, from a graph read — the panel's own
  // copy of the standing, re-read whenever the graph says it moved.
  const [standings, setStandings] = useState<Map<Id, number> | null>(null);
  // The actors with a respond in flight, so that row's buttons disable while it settles.
  const [acting, setActing] = useState<ReadonlySet<Id>>(new Set());

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      setItems(await client.notifications.listNotifications(PAGE_SIZE));
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // A pushed notification is a cue to reconcile, never the row itself; re-read the page.
  useEffect(() => {
    if (!client) {
      return;
    }
    return client.notifications.onNotification(() => {
      void reload();
    });
  }, [client, reload]);

  // The actors a friend-request row points at. Only these make the graph worth reading here.
  const friendActorIds = useMemo(() => {
    const ids = new Set<Id>();
    for (const item of items ?? []) {
      if (item.kind === FRIEND_REQUEST_KIND && item.actorId !== undefined) {
        ids.add(item.actorId);
      }
    }
    return [...ids];
  }, [items]);

  // The standing of those actors: pending-incoming means the row may offer its answer, anything
  // else (friend, outgoing, gone) means it may not. A failed read leaves the buttons off rather
  // than guessing — the inbox still renders.
  const reloadStandings = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const entries = await client.social.listRelationships();
      setStandings(new Map(entries.map((entry) => [entry.userId, entry.kind])));
    } catch {
      // An unread standing is an inert row, not a wrong button.
    }
  }, [client]);

  useEffect(() => {
    if (friendActorIds.length === 0) {
      return;
    }
    void reloadStandings();
  }, [friendActorIds, reloadStandings]);

  // The graph moves when a request is answered anywhere (this panel, the Friends tab, another
  // device); re-read the standing debounced, the same quiet window the Friends surfaces use, so a
  // burst of per-device echoes answers one read.
  useEffect(() => {
    if (!client || friendActorIds.length === 0) {
      return;
    }
    const reRead = debounce(() => void reloadStandings(), FRIEND_EVENT_DEBOUNCE_MS);
    const off = client.social.onFriendEvent(reRead);
    return () => {
      off();
      reRead.cancel();
    };
  }, [client, friendActorIds, reloadStandings]);

  /** Answers a friend request from its row: the wire call, then the standing and inbox re-reads. */
  async function respond(actorId: Id, accept: boolean): Promise<void> {
    if (!client || acting.has(actorId)) {
      return;
    }
    setActing((prev) => new Set(prev).add(actorId));
    try {
      await client.social.friendRespond(actorId, accept);
      await reloadStandings();
      await reload();
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setActing((prev) => {
        const next = new Set(prev);
        next.delete(actorId);
        return next;
      });
    }
  }

  async function onMarkAllRead(): Promise<void> {
    if (!client || items === null || items.length === 0 || busy) {
      return;
    }
    setBusy(true);
    try {
      const newest = items.reduce((left, right) => (right.at > left.at ? right : left));
      await client.notifications.acknowledgeNotifications(newest.at);
      await reload();
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setBusy(false);
    }
  }

  const actorIds = useMemo(
    () => [
      ...new Set(
        (items ?? []).map((item) => item.actorId).filter((id): id is Id => id !== undefined),
      ),
    ],
    [items],
  );
  const profiles = useProfiles(actorIds);

  return (
    <div className="panel">
      <header className="panel-head">
        <h1 className="panel-title">Notifications</h1>
        <button
          type="button"
          className="btn"
          disabled={busy || items === null || items.length === 0}
          onClick={() => void onMarkAllRead()}
        >
          {busy ? <Spinner /> : 'Mark all read'}
        </button>
      </header>

      {error ? <p className="form-error">{error}</p> : null}

      {items === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : items.length === 0 ? (
        <div className="center-fill">
          <div>
            <div className="emoji">
              <Icon name="bell" size={24} />
            </div>
            You are all caught up.
          </div>
        </div>
      ) : (
        <ul className="notification-list">
          {items.map((item) => (
            <NotificationRow
              key={item.id}
              item={item}
              actorName={
                item.actorId !== undefined
                  ? (profiles.get(item.actorId)?.displayName ?? null)
                  : null
              }
              actions={
                item.kind === FRIEND_REQUEST_KIND &&
                item.actorId !== undefined &&
                standings?.get(item.actorId) === KIND_PENDING_INCOMING ? (
                  <FriendRequestActions
                    busy={acting.has(item.actorId)}
                    onAccept={() => void respond(item.actorId as Id, true)}
                    onDecline={() => void respond(item.actorId as Id, false)}
                  />
                ) : undefined
              }
            />
          ))}
        </ul>
      )}
    </div>
  );
}

/** Renders one inbox row: actor avatar, a humanised kind, a relative time, and its answer if the
 * wire carries one.
 *
 * Exported presentational over plain props, so what a row offers — the sentence for every kind,
 * action buttons only when the caller (who has read the graph) says the request is still pending —
 * is testable without a live client. */
export function NotificationRow({
  item,
  actorName,
  actions,
}: {
  item: InboxItem;
  actorName: string | null;
  /** The row's answer, when its kind and standing allow one; every other kind renders none. */
  actions?: ReactNode;
}): ReactNode {
  const title =
    actorName !== null && actorName.length > 0
      ? `${actorName} — ${kindLabel(item.kind)}`
      : kindLabel(item.kind);
  return (
    <li className="notification-row">
      <Avatar name={actorName ?? kindLabel(item.kind)} id={item.actorId ?? item.id} size={36} />
      <div className="person-main">
        <span className="person-name">{title}</span>
        {item.title ? <span className="person-note">{item.title}</span> : null}
      </div>
      {actions ? <div className="person-actions">{actions}</div> : null}
      <time className="person-note" dateTime={new Date(item.at).toISOString()}>
        {formatRelative(item.at)}
      </time>
    </li>
  );
}

/**
 * The answer a pending friend request carries in its row: Accept and Decline, disabled together
 * while the one wire call they share is in flight.
 *
 * Exported presentational over plain props, so the pair — and its busy state — is testable
 * without a live client.
 */
export function FriendRequestActions({
  busy,
  onAccept,
  onDecline,
}: {
  busy: boolean;
  onAccept: () => void;
  onDecline: () => void;
}): ReactNode {
  return (
    <>
      <button type="button" className="btn btn-primary btn-sm" disabled={busy} onClick={onAccept}>
        Accept
      </button>
      <button type="button" className="btn btn-ghost btn-sm" disabled={busy} onClick={onDecline}>
        Decline
      </button>
    </>
  );
}

/**
 * The inbox `kind` is the wire's own word: the {@link NotificationKind} enum's number in a string
 * ("4"), or a snake_case word should a newer server say one. Name the number through the enum the
 * client already carries, then render it as a sentence — spaced words, a leading capital, the rest
 * lowercase — so a row reads "Friend request" rather than the wire's arithmetic. Anything unknown
 * keeps the wire's own word, so a kind this build has no name for still reads sanely.
 */
function kindLabel(kind: string): string {
  const numeric = Number(kind);
  const named =
    Number.isInteger(numeric) && numeric > 0 ? (NotificationKind[numeric] ?? null) : null;
  const source = named ?? kind;
  const spaced = source
    .replaceAll('_', ' ')
    .replace(/([a-z])([A-Z])/g, '$1 $2')
    .toLowerCase();
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}
