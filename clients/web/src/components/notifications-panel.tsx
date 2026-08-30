'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import type { Id, InboxItem } from '@migo/sdk';

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
 */
export function NotificationsPanel(): ReactNode {
  const { client } = useMigo();

  const [items, setItems] = useState<InboxItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

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
            />
          ))}
        </ul>
      )}
    </div>
  );
}

/** Renders one inbox row: actor avatar, a humanised kind, and a relative time. */
function NotificationRow({
  item,
  actorName,
}: {
  item: InboxItem;
  actorName: string | null;
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
      <time className="person-note" dateTime={new Date(item.at).toISOString()}>
        {formatRelative(item.at)}
      </time>
    </li>
  );
}

/**
 * The inbox `kind` is a snake_case wire word (`friend_request`); render it as spaced words with a
 * leading capital. It is a closed server vocabulary, so anything unknown still reads sanely.
 */
function kindLabel(kind: string): string {
  const spaced = kind.replaceAll('_', ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}
