'use client';

/**
 * The phone's home screen.
 *
 * Below the PC breakpoint there is no desk: Friends, Rooms, and Feed are home views the strip
 * navigates between, rendered full-bleed under it. The orange me card at the top is the account
 * (its avatar opens the account sheet, its status line edits in place, its chips reach the
 * messages and the account menu); below it the view header and the list. People and rooms are
 * never listed with desktop double-clicks — a tap opens an intent sheet, and the sheet's actions
 * are the ones the wire really carries.
 *
 * All three views read the real client: the friends list is the relationship graph, the rooms view
 * is the account's group chats plus the public directory, and the feed is the activity stream
 * panel itself.
 */

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { ConversationKind, PresenceState, RelationshipKind } from '@migo/sdk';
import type { Id, RelationshipEntry, RoomSummary } from '@migo/sdk';

import { useConversations } from '@/lib/migo/conversations-provider.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMePresence } from '@/lib/migo/use-me-presence.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { usePresenceOf } from '@/lib/migo/use-presence.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import { ListFooter } from './list-footer.js';
import { NewConversationDialog } from './new-conversation-dialog.js';
import { SpacePanel } from './space-panel.js';
import { Spinner } from './spinner.js';
import { PresencePill, Sheet, SheetAction, presenceColor, presenceName } from './intent-sheet.js';
import type { MobileNavTab } from './mobile-tab-bar.js';
import type { WinKind } from './window-types.js';

/** The relationship kind as a plain number, so the filter compares number to number. */
const KIND_FRIEND: number = RelationshipKind.Friend;

/** How many rooms one directory read asks for. */
const ROOMS_PAGE = 30;

/** What the me card's status edit accepts, matching the profile field's bound. */
const STATUS_MAX_CHARS = 100;

/** The self-reportable states, in the sheet's 2×2 order. */
const PRESENCE_GRID: ReadonlyArray<PresenceState> = [
  PresenceState.Online,
  PresenceState.Busy,
  PresenceState.Away,
  PresenceState.Invisible,
];

export function MobileHome({
  nav,
  onOpenConversation,
  onOpenWindow,
  onOpenUserIntent,
  onOpenRoomIntent,
  onRequestLogout,
}: {
  nav: MobileNavTab;
  onOpenConversation: (conversationId: Id) => void;
  /** Opens one of the app's windows — the me sheet's and the view headers' action. */
  onOpenWindow: (kind: Exclude<WinKind, 'chat'>) => void;
  /** A tap on a person: the parent opens the user intent sheet. */
  onOpenUserIntent: (userId: Id) => void;
  /** A tap on a room: the parent opens the room intent sheet. */
  onOpenRoomIntent: (room: RoomSummary) => void;
  onRequestLogout: () => void;
}): ReactNode {
  const { client, accountId } = useMigo();
  const me = useMePresence();
  const { items, unread } = useConversations();
  const rooms = useRooms();

  const [meOpen, setMeOpen] = useState(false);
  const [statusEditing, setStatusEditing] = useState(false);
  const [statusDraft, setStatusDraft] = useState('');
  const [query, setQuery] = useState('');
  const [friends, setFriends] = useState<RelationshipEntry[] | null>(null);
  const [friendsError, setFriendsError] = useState<string | null>(null);
  const [directory, setDirectory] = useState<RoomSummary[] | null>(null);
  const [groupDialogOpen, setGroupDialogOpen] = useState(false);

  // The relationship graph — the same read the Friends panel performs, refreshed on every friend
  // event because the event says the graph moved, not how.
  const reloadFriends = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      setFriends(await client.social.listRelationships());
      setFriendsError(null);
    } catch (cause) {
      setFriendsError(friendlyError(cause));
    }
  }, [client]);

  useEffect(() => {
    void reloadFriends();
  }, [reloadFriends]);

  useEffect(() => {
    if (!client) {
      return;
    }
    return client.social.onFriendEvent(() => {
      void reloadFriends();
    });
  }, [client, reloadFriends]);

  // The public directory, read once per visit; the room records the shell already watches overlay
  // the live counts on the rows.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client.rooms
      .list(ROOMS_PAGE)
      .then((response) => {
        if (!cancelled) {
          setDirectory(response.rooms);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setDirectory([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  const friendEntries = useMemo(
    () => friends?.filter((entry) => entry.kind === KIND_FRIEND) ?? null,
    [friends],
  );
  const friendIds = useMemo(
    () => friendEntries?.map((entry) => entry.userId) ?? [],
    [friendEntries],
  );
  const profiles = useProfiles(friendIds);
  const presence = usePresenceOf(friendIds, profiles);

  const visibleFriends = useMemo(() => {
    if (friendEntries === null) {
      return null;
    }
    const needle = query.trim().toLowerCase();
    return friendEntries.filter((entry) => {
      if (needle.length > 0) {
        const profile = profiles.get(entry.userId);
        const name = profile?.displayName ?? profile?.username ?? entry.userId;
        return name.toLowerCase().includes(needle);
      }
      return true;
    });
  }, [friendEntries, profiles, query]);

  const groups = items.filter((item) => item.kind === ConversationKind.Group);

  // The mail chip's badge: a live unread mark or a summary whose persisted read mark lags.
  const unreadTotal = items.filter(
    (item) => unread.has(item.conversationId) || item.lastSeq > item.readSeq,
  ).length;

  const onlineCount =
    visibleFriends?.filter((entry) => presence.get(entry.userId) === PresenceState.Online).length ??
    0;

  const viewTitle =
    nav === 'friends'
      ? `Friends · ${onlineCount}/${friendEntries?.length ?? 0} online`
      : nav === 'rooms'
        ? `Rooms · ${groups.length} group${groups.length === 1 ? '' : 's'} · ${directory?.length ?? 0} public`
        : 'Recent activity';

  function commitStatus(): void {
    me.publish(me.presence, statusDraft.trim());
    setStatusEditing(false);
  }

  /** An action that leaves the sheet first, then does what it came for. */
  function fromSheet(action: () => void): void {
    setMeOpen(false);
    action();
  }

  const footerHint =
    nav === 'rooms'
      ? 'tap a room to see options'
      : nav === 'friends'
        ? 'tap a friend to see options'
        : 'Migo activity';

  return (
    <div className="mhome">
      <div className="win-frame mhome-frame">
        {/* ---- me card (orange) ---- */}
        <div className="hdr-orange mhome-me">
          <button
            type="button"
            className="me-avatar-ring me-avatar-button"
            onClick={() => setMeOpen(true)}
            aria-label="Open my account sheet"
            title="My account"
          >
            <Avatar
              name={me.displayName}
              id={accountId ?? 'me'}
              size={46}
              avatarUrl={me.avatarUrl}
            />
          </button>

          <div className="me-main">
            <div className="me-name-row">
              <span className="blink-dot" style={{ background: presenceColor(me.presence) }} />
              <span className="me-name me-name-lg">{me.displayName}</span>
            </div>
            {statusEditing ? (
              <input
                autoFocus
                className="hdr-status-input"
                value={statusDraft}
                maxLength={STATUS_MAX_CHARS}
                placeholder="Set a status..."
                onChange={(event) => setStatusDraft(event.target.value)}
                onBlur={commitStatus}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') {
                    commitStatus();
                  }
                }}
                aria-label="Edit your status"
              />
            ) : (
              <button
                type="button"
                className="me-status"
                onClick={() => {
                  setStatusDraft(me.status);
                  setStatusEditing(true);
                }}
                title="Tap to edit your status"
              >
                {me.status.length > 0 ? me.status : 'New here! Say hi :)'}
              </button>
            )}
          </div>

          <div className="me-chips">
            <button
              type="button"
              className="hdr-chip hdr-chip-icon"
              onClick={() => onOpenWindow('notifications')}
              aria-label="Messages"
              title="Messages"
            >
              <Icon name="bell" size={14} />
              {unreadTotal > 0 ? (
                <span className="hdr-chip-badge">{unreadTotal > 9 ? '9+' : unreadTotal}</span>
              ) : null}
            </button>
            <button
              type="button"
              className="hdr-chip hdr-chip-icon"
              onClick={() => setMeOpen(true)}
              aria-label="Account menu"
              title="Account & settings"
            >
              <Icon name="settings" size={15} />
            </button>
          </div>
        </div>

        {/* ---- view header ---- */}
        <div className="gloss-panel mhome-viewhead">
          <span className="mhome-view-title">{viewTitle}</span>
          {nav === 'friends' ? (
            <>
              <button
                type="button"
                className="tbtn tbtn-sm"
                onClick={() => onOpenWindow('search')}
                aria-label="Search people"
                title="Search people"
              >
                <Icon name="search" size={17} />
              </button>
              <button
                type="button"
                className="tbtn tbtn-sm"
                onClick={() => setGroupDialogOpen(true)}
                aria-label="New conversation"
                title="New conversation"
              >
                <Icon name="user-plus" size={17} />
              </button>
            </>
          ) : null}
          {nav === 'rooms' ? (
            <>
              <button
                type="button"
                className="tbtn tbtn-sm"
                onClick={() => setGroupDialogOpen(true)}
                aria-label="New group chat"
                title="New group chat"
              >
                <Icon name="user-plus" size={17} />
              </button>
              <button
                type="button"
                className="tbtn tbtn-sm"
                onClick={() => onOpenWindow('search')}
                aria-label="Search rooms"
                title="Search rooms"
              >
                <Icon name="rooms" size={17} />
              </button>
            </>
          ) : null}
        </div>

        {/* ---- body ---- */}
        <div className="win-body retro-scroll mhome-body">
          {/* ===== FRIENDS ===== */}
          {nav === 'friends' ? (
            <>
              <div className="mhome-search">
                <Icon name="search" size={15} className="mhome-search-icon" />
                <input
                  className="mhome-search-input"
                  placeholder="Search friends..."
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  aria-label="Search friends"
                />
                {query.length > 0 ? (
                  <button
                    type="button"
                    className="mhome-search-clear"
                    onClick={() => setQuery('')}
                    aria-label="Clear search"
                  >
                    <Icon name="close" size={14} />
                  </button>
                ) : null}
              </div>
              {friendsError !== null ? <div className="list-hint">{friendsError}</div> : null}
              {visibleFriends === null ? (
                <div className="mhome-loading">
                  <Spinner />
                </div>
              ) : (
                <>
                  {visibleFriends.map((entry) => {
                    const profile = profiles.get(entry.userId);
                    const state = presence.get(entry.userId);
                    const name = profile?.displayName ?? profile?.username ?? entry.userId;
                    return (
                      <button
                        key={entry.userId}
                        type="button"
                        className="mhome-row"
                        onClick={() => onOpenUserIntent(entry.userId)}
                      >
                        <Avatar
                          name={name}
                          id={entry.userId}
                          size={44}
                          avatarUrl={profile?.avatarUrl}
                          presence={state}
                        />
                        <span className="mhome-row-main">
                          <span className="mhome-row-name">{name}</span>
                          <span className="mhome-row-sub">
                            {profile?.customStatus ?? presenceName(state)}
                          </span>
                        </span>
                        <Icon name="chevron-right" size={18} className="mhome-row-go" />
                      </button>
                    );
                  })}
                  {visibleFriends.length === 0 ? (
                    <div className="mhome-empty">
                      <Icon name="friends" size={30} />
                      <span>
                        {query.trim().length > 0
                          ? 'No friends match your search.'
                          : 'No friends yet — add someone from their profile.'}
                      </span>
                    </div>
                  ) : null}
                </>
              )}
            </>
          ) : null}

          {/* ===== ROOMS ===== */}
          {nav === 'rooms' ? (
            <>
              <div className="list-section-head list-section-head-row">
                <span>Your groups ({groups.length})</span>
                <button
                  type="button"
                  className="list-section-action"
                  onClick={() => setGroupDialogOpen(true)}
                >
                  <Icon name="user-plus" size={13} /> New group
                </button>
              </div>
              {groups.map((group) => (
                <button
                  key={group.conversationId}
                  type="button"
                  className="mhome-row"
                  onClick={() => onOpenConversation(group.conversationId)}
                >
                  <span className="part-chip part-chip-group">
                    <Icon name="chats" size={24} />
                  </span>
                  <span className="mhome-row-main">
                    <span className="mhome-row-title">
                      <span className="mhome-row-name">{group.title ?? 'Group'}</span>
                      <span className="room-count">
                        <b>{group.members?.length ?? 1}</b> members
                      </span>
                    </span>
                    <span className="mhome-row-sub">Private group chat — tap to open</span>
                  </span>
                  <Icon name="chevron-right" size={18} className="mhome-row-go" />
                </button>
              ))}
              {groups.length === 0 ? (
                <div className="list-hint">
                  No groups yet — tap <b>New group</b> to start one with your friends.
                </div>
              ) : null}

              <div className="list-section-head">Public rooms ({directory?.length ?? 0})</div>
              {directory === null ? (
                <div className="mhome-loading">
                  <Spinner />
                </div>
              ) : (
                <>
                  {directory.map((room) => {
                    const live = rooms.liveFor(room.roomId);
                    const users = live?.onlineCount ?? room.onlineCount;
                    const max = room.maxMembers ?? live?.maxMembers ?? 0;
                    const pct = max > 0 ? Math.min(100, Math.round((users / max) * 100)) : 0;
                    const nearFull = pct >= 85;
                    return (
                      <button
                        key={room.roomId}
                        type="button"
                        className="mhome-row mhome-row-room"
                        onClick={() => onOpenRoomIntent(room)}
                      >
                        <span className="part-chip part-chip-room">
                          <Icon name="rooms" size={25} />
                        </span>
                        <span className="mhome-row-main">
                          <span className="mhome-row-title">
                            <span className="mhome-row-name">{room.name}</span>
                            {max > 0 ? (
                              <span className={`room-count${nearFull ? ' room-count-full' : ''}`}>
                                <b>{users}</b>/{max}
                              </span>
                            ) : null}
                          </span>
                          <span className="mhome-row-sub">
                            {room.topic ?? room.description ?? 'A public room'}
                          </span>
                          {max > 0 ? (
                            <span className="mhome-occupancy" aria-hidden="true">
                              <span
                                style={{
                                  width: `${pct}%`,
                                  background: nearFull
                                    ? 'var(--migo-orange)'
                                    : 'var(--migo-teal-hover)',
                                }}
                              />
                            </span>
                          ) : null}
                        </span>
                        <Icon name="chevron-right" size={18} className="mhome-row-go" />
                      </button>
                    );
                  })}
                  {directory.length === 0 ? (
                    <div className="mhome-empty">
                      <Icon name="rooms" size={30} />
                      <span>No public rooms on this server yet.</span>
                    </div>
                  ) : null}
                </>
              )}
            </>
          ) : null}

          {/* ===== FEED ===== */}
          {nav === 'feed' ? <SpacePanel onOpenConversation={onOpenConversation} /> : null}
        </div>

        {/* ---- footer ---- */}
        <ListFooter tab={nav} hint={footerHint} />
      </div>

      {/* ---- me sheet (account) ---- */}
      <Sheet open={meOpen} onClose={() => setMeOpen(false)} title="My account">
        <div className="sheet-target">
          <span className="sheet-target-avatar">
            <Avatar
              name={me.displayName}
              id={accountId ?? 'me'}
              size={54}
              avatarUrl={me.avatarUrl}
            />
            <span
              className="sheet-target-dot"
              style={{ background: presenceColor(me.presence) }}
              aria-hidden="true"
            />
          </span>
          <span className="sheet-target-main">
            <span className="sheet-target-name">{me.displayName}</span>
            {me.username.length > 0 ? (
              <span className="sheet-target-sub">@{me.username}</span>
            ) : null}
            <span className="sheet-target-sub">
              <span
                className="sheet-target-presence"
                style={{ background: presenceColor(me.presence) }}
                aria-hidden="true"
              />
              {presenceName(me.presence)}
            </span>
          </span>
        </div>

        <div className="sheet-label">Presence</div>
        <div className="presence-grid">
          {PRESENCE_GRID.map((state) => (
            <PresencePill
              key={state}
              state={state}
              current={me.presence}
              onPick={(next) => me.publish(next, me.status)}
            />
          ))}
        </div>

        <div className="sheet-sep" />
        <SheetAction
          icon="user"
          label="My Profile"
          onClick={() => fromSheet(() => onOpenWindow('profile'))}
        />
        <SheetAction
          icon="settings"
          label="Edit Profile & Settings"
          onClick={() => fromSheet(() => onOpenWindow('settings'))}
        />
        <SheetAction
          icon="wallet"
          label="My Credits & TopUp"
          onClick={() => fromSheet(() => onOpenWindow('wallet'))}
        />
        <SheetAction
          icon="bell"
          label="Messages"
          sub={unreadTotal > 0 ? `${unreadTotal} unread` : undefined}
          onClick={() => fromSheet(() => onOpenWindow('notifications'))}
        />
        <SheetAction
          icon="gift"
          label="Store"
          sub="Emoticons · Stickers · Gifts"
          onClick={() => fromSheet(() => onOpenWindow('store'))}
        />
        <SheetAction
          icon="game"
          label="Games"
          onClick={() => fromSheet(() => onOpenWindow('games'))}
        />
        <SheetAction
          icon="search"
          label="Search"
          onClick={() => fromSheet(() => onOpenWindow('search'))}
        />

        <div className="sheet-sep" />
        <SheetAction
          icon="signout"
          label="Log out"
          danger
          onClick={() => fromSheet(onRequestLogout)}
        />
        <div className="sheet-tail" />
      </Sheet>

      {/* New conversation / group dialog */}
      {groupDialogOpen ? <NewConversationDialog onClose={() => setGroupDialogOpen(false)} /> : null}
    </div>
  );
}
