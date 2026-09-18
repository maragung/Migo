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
 * All three views read the real client: the friends view is the relationship graph — friends,
 * pending requests with their accept and decline, and the graph's suggestions, plus a server-side
 * username search behind the header's icon — the rooms view is the public directory with its own
 * wire search and create-room flow, and the feed is the activity stream panel itself.
 *
 * Chat List Mode makes its fourth view, `main`, the home screen itself: the conversation list,
 * which a tap opens as the phone's full-screen chat activity. The mode's strip leads with the
 * Main tab for it, and the view header's back control is the other way back — one step from
 * whichever tabbed view is on screen. The other three views and their behaviour are untouched
 * by it.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { ConversationKind, PresenceState, RelationshipKind } from '@migo/sdk';
import type { Id, RelationshipEntry, RoomSummary, SuggestedUser } from '@migo/sdk';

import { debounce } from '@/lib/debounce.js';
import { useBalance } from '@/lib/migo/use-balance.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMePresence } from '@/lib/migo/use-me-presence.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { usePresenceOf } from '@/lib/migo/use-presence.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';

import { Avatar } from './avatar.js';
import { BotBadge } from './bot-badge.js';
import { ConversationList } from './conversation-list.js';
import { CreateRoomDialog } from './create-room-dialog.js';
import { FriendsSearch } from './friends-panel.js';
import { CoinMark, Icon } from './icons.js';
import { ListFooter } from './list-footer.js';
import { NewConversationDialog } from './new-conversation-dialog.js';
import { SpacePanel } from './space-panel.js';
import { Spinner } from './spinner.js';
import { PresencePill, Sheet, SheetAction, presenceColor, presenceName } from './intent-sheet.js';
import type { MobileNavTab } from './mobile-tab-bar.js';
import type { WinKind } from './window-types.js';

/** The relationship kinds as plain numbers, so the filters compare number to number. */
const KIND_FRIEND: number = RelationshipKind.Friend;
const KIND_PENDING_INCOMING: number = RelationshipKind.PendingIncoming;
const KIND_PENDING_OUTGOING: number = RelationshipKind.PendingOutgoing;

/** How long a friend-event re-read waits for the events to stop arriving (see the debounce). */
const FRIEND_EVENT_DEBOUNCE_MS = 300;

/** How many rooms one directory read asks for. */
const ROOMS_PAGE = 30;

/** How many people one username search asks the server for — one small page, not the directory. */
const PEOPLE_SEARCH_LIMIT = 20;

/** The pause that turns the rooms field's live text into the wire's query. */
const ROOMS_SEARCH_DEBOUNCE_MS = 300;

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
  onBackToChats,
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
  /**
   * Chat List Mode's quick way back: the view header shows it on the three tabbed views, and it
   * returns the phone to the conversation list that is the mode's home screen — the same screen
   * the strip's Main tab opens, reached in one step instead of a strip trip. Undefined in the
   * tabbed layout, whose home is the strip's own tabs.
   */
  onBackToChats?: () => void;
  onRequestLogout: () => void;
}): ReactNode {
  const { client, accountId } = useMigo();
  const me = useMePresence();
  const { items, unread } = useConversations();
  const rooms = useRooms();
  const balance = useBalance();

  const [meOpen, setMeOpen] = useState(false);
  const [statusEditing, setStatusEditing] = useState(false);
  const [statusDraft, setStatusDraft] = useState('');
  const [friends, setFriends] = useState<RelationshipEntry[] | null>(null);
  const [friendsError, setFriendsError] = useState<string | null>(null);
  const [suggestions, setSuggestions] = useState<SuggestedUser[]>([]);
  // One stable action per person, so a row's buttons can disable while that person's call is in
  // flight — the same bargain the Friends panel's `busy` set makes.
  const [socialBusy, setSocialBusy] = useState<ReadonlySet<Id>>(new Set());
  // The people search: the field hides behind its icon in the view header (the same reveal the
  // Friends panel's head uses), and the results replace the list until the search is finished.
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [searchResults, setSearchResults] = useState<SuggestedUser[] | null>(null);
  const [directory, setDirectory] = useState<RoomSummary[] | null>(null);
  const [groupDialogOpen, setGroupDialogOpen] = useState(false);
  const [roomDialogOpen, setRoomDialogOpen] = useState(false);
  // The rooms search: the debounced query is what the wire sees, the live text what the user sees.
  const [roomQuery, setRoomQuery] = useState('');
  const [roomLiveQuery, setRoomLiveQuery] = useState('');
  const roomDebounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // The relationship graph and the suggestions beside it — the same reads the Friends panel
  // performs, refreshed on every friend event because the event says the graph moved, not how (a
  // new friend changes what is suggested, too).
  const reloadFriends = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const [relationships, suggested] = await Promise.all([
        client.social.listRelationships(),
        client.social.suggestions(),
      ]);
      setFriends(relationships);
      setSuggestions(suggested);
      setFriendsError(null);
    } catch (cause) {
      setFriendsError(friendlyError(cause));
    }
  }, [client]);

  useEffect(() => {
    void reloadFriends();
  }, [reloadFriends]);

  // The friend events re-read the graph debounced: one acceptance can arrive as several events
  // (each echoed per device), and a read per event would ask the same question the last event
  // already answers.
  useEffect(() => {
    if (!client) {
      return;
    }
    const reRead = debounce(() => void reloadFriends(), FRIEND_EVENT_DEBOUNCE_MS);
    const off = client.social.onFriendEvent(reRead);
    return () => {
      off();
      reRead.cancel();
    };
  }, [client, reloadFriends]);

  // The public directory, read per visit and per rooms query — the tab owns its own search, so
  // the query goes to the wire here rather than hopping out to the Search window; the room
  // records the shell already watches overlay the live counts on the rows.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    const text = roomQuery.trim();
    client.rooms
      .list(ROOMS_PAGE, text.length > 0 ? { query: text } : undefined)
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
  }, [client, roomQuery]);

  // The rooms field's live text becomes the wire's query after a pause in typing — one round trip
  // per intent, not per keystroke, against a rate-limited endpoint.
  function onRoomSearchInput(value: string): void {
    setRoomLiveQuery(value);
    if (roomDebounceRef.current !== null) {
      clearTimeout(roomDebounceRef.current);
    }
    roomDebounceRef.current = setTimeout(() => setRoomQuery(value), ROOMS_SEARCH_DEBOUNCE_MS);
  }

  const friendEntries = useMemo(
    () => friends?.filter((entry) => entry.kind === KIND_FRIEND) ?? null,
    [friends],
  );
  // The pending halves of the graph: incoming requests (the ones this account can answer) and
  // outgoing ones (sent, waiting). Rendered between the friends and the groups, where a phone
  // reaches them without leaving the tab.
  const incoming = useMemo(
    () => friends?.filter((entry) => entry.kind === KIND_PENDING_INCOMING) ?? [],
    [friends],
  );
  const outgoing = useMemo(
    () => friends?.filter((entry) => entry.kind === KIND_PENDING_OUTGOING) ?? [],
    [friends],
  );
  const friendIds = useMemo(
    () => friendEntries?.map((entry) => entry.userId) ?? [],
    [friendEntries],
  );
  // Names for every row the Friends view draws — friends and requesters both — through the shared
  // profile cache; presence stays a friends-only question (a pending request has none to show).
  const relatedIds = useMemo(() => friends?.map((entry) => entry.userId) ?? [], [friends]);
  const profiles = useProfiles(relatedIds);
  const presence = usePresenceOf(friendIds, profiles);

  const groups = items.filter((item) => item.kind === ConversationKind.Group);

  /** Runs one social action for a person, disabling that person's buttons until it settles. */
  async function socialAct(userId: Id, action: () => Promise<void>): Promise<void> {
    if (!client) {
      return;
    }
    setSocialBusy((prev) => new Set(prev).add(userId));
    try {
      await action();
      await reloadFriends();
    } catch (cause) {
      setFriendsError(friendlyError(cause));
    } finally {
      setSocialBusy((prev) => {
        const next = new Set(prev);
        next.delete(userId);
        return next;
      });
    }
  }

  /** The answer to an incoming request: the wire call, then the graph re-read. */
  function respond(userId: Id, accept: boolean): void {
    void socialAct(userId, () =>
      client ? client.social.friendRespond(userId, accept) : Promise.resolve(),
    );
  }

  /** The ask of a suggested (or searched) person: the wire call, then the graph re-read. */
  function requestFriend(userId: Id): void {
    void socialAct(userId, () =>
      client ? client.social.friendRequest(userId) : Promise.resolve(),
    );
  }

  // The people search follows the Friends panel's bargain exactly: submit is the ask (an emptied
  // field is a return to the list, not a search for nothing), and the field's dismissal takes the
  // results with it — no orphaned results with no field left to change them.
  async function onPeopleSearch(event: FormEvent<HTMLFormElement>): Promise<void> {
    event.preventDefault();
    const text = searchQuery.trim();
    if (!client) {
      return;
    }
    if (text.length === 0) {
      setSearchResults(null);
      return;
    }
    try {
      setSearchResults(await client.social.search(text, PEOPLE_SEARCH_LIMIT));
      setFriendsError(null);
    } catch (cause) {
      setFriendsError(friendlyError(cause));
    }
  }

  function dismissPeopleSearch(): void {
    setSearchOpen(false);
    setSearchQuery('');
    setSearchResults(null);
  }

  // The mail chip's badge: a live unread mark or a summary whose persisted read mark lags.
  const unreadTotal = items.filter(
    (item) => unread.has(item.conversationId) || item.lastSeq > item.readSeq,
  ).length;

  const onlineCount =
    friendEntries?.filter((entry) => presence.get(entry.userId) === PresenceState.Online).length ??
    0;

  const viewTitle =
    nav === 'main'
      ? `Chats · ${items.length} conversation${items.length === 1 ? '' : 's'}`
      : nav === 'friends'
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
        : nav === 'main'
          ? 'tap a conversation to open it'
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

          {/* The chips stack rather than sit in a row: the wallet's $MIG figure rides above the
              alerts and account controls, where the design now puts it (it used to live in the
              footer band below, which the connection mark has taken), and the icon buttons the
              figure watches over are the me card's two doors — the alerts and the account menu. */}
          <div className="me-chips">
            <span
              className="me-balance"
              title="$MIG balance"
              aria-label="$MIG balance — open My Wallet from the account menu"
            >
              <CoinMark size={14} />
              {/* An unread balance says nothing rather than zero: a wallet that failed to load is
                  not an empty one, and the difference matters to whoever is about to spend. */}
              <span>{balance !== null ? `${balance.toLocaleString()} $MIG` : '$MIG'}</span>
            </span>
            <div className="me-chip-row">
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
        </div>

        {/* ---- view header ---- */}
        <div className="gloss-panel mhome-viewhead">
          {/* Chat List Mode's quick way home: the strip's Main tab reaches the list too — this
              is the one-step way back from the view on screen, without a trip to the strip. */}
          {onBackToChats !== undefined && nav !== 'main' ? (
            <button
              type="button"
              className="tbtn tbtn-sm"
              onClick={onBackToChats}
              aria-label="Back to chats"
              title="Back to chats"
            >
              <Icon name="chevron-left" size={17} />
            </button>
          ) : null}
          <span className="mhome-view-title">{viewTitle}</span>
          {nav === 'friends' ? (
            <>
              {/* The people search is the same bargain the Friends panel's head makes: the field
                  waits behind its icon until someone wants it, arrives focused in the icon's
                  place, and leaves when the search is finished — wearing the header's own white
                  ink rather than the panel's. The ask goes to the wire (a username prefix), not
                  a local filter, so a person who is not a friend yet can be found and asked. */}
              <FriendsSearch
                tone="home"
                open={searchOpen}
                active={searchResults !== null}
                query={searchQuery}
                onQueryChange={setSearchQuery}
                onSubmit={(event) => void onPeopleSearch(event)}
                onReveal={() => setSearchOpen(true)}
                onDismiss={dismissPeopleSearch}
              />
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
          {/* The Rooms tab owns its own search and its own way in: the field queries the
              directory on the wire (debounced — a pause is the ask, not a keystroke), and the
              plus opens the same Create Room dialog the desktop's Rooms panel uses, so a room is
              never more than this tab away. */}
          {nav === 'rooms' ? (
            <>
              <input
                type="search"
                className="mhome-viewhead-search"
                placeholder="Search rooms"
                value={roomLiveQuery}
                onChange={(event) => onRoomSearchInput(event.target.value)}
                aria-label="Search rooms"
              />
              <button
                type="button"
                className="tbtn tbtn-sm"
                onClick={() => setRoomDialogOpen(true)}
                aria-label="New room"
                title="New room"
              >
                <Icon name="plus" size={17} />
              </button>
            </>
          ) : null}
        </div>

        {/* ---- body ---- */}
        <div className="win-body retro-scroll mhome-body">
          {/* ===== MAIN (Chat List Mode) ===== */}
          {/* The conversation list itself, unchanged from the surface every other client lists
              chats on — the mode makes it the home screen the strip's Main tab opens, not a
              second list beside the tabbed views. A tap opens the thread as the phone's
              full-screen chat activity, and its back control returns here. */}
          {nav === 'main' ? <ConversationList /> : null}

          {/* ===== FRIENDS ===== */}
          {nav === 'friends' ? (
            <>
              {friendsError !== null ? <div className="list-hint">{friendsError}</div> : null}
              {searchResults !== null ? (
                /* The search's results replace the list until the search is finished — the field
                   above them is the only way to change them, and dismissing it takes them away. */
                <>
                  <div className="list-section-head">Search results</div>
                  {searchResults.map((person) => (
                    <SuggestionRow
                      key={person.accountId}
                      person={person}
                      busy={socialBusy.has(person.accountId)}
                      onRequest={() => requestFriend(person.accountId)}
                    />
                  ))}
                  {searchResults.length === 0 ? (
                    <div className="mhome-empty">
                      <Icon name="friends" size={30} />
                      <span>No one found for “{searchQuery.trim()}”.</span>
                    </div>
                  ) : null}
                </>
              ) : friendEntries === null ? (
                <div className="mhome-loading">
                  <Spinner />
                </div>
              ) : (
                <>
                  {friendEntries.map((entry) => {
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
                  {friendEntries.length === 0 ? (
                    <div className="mhome-empty">
                      <Icon name="friends" size={30} />
                      <span>No friends yet — add someone from the suggestions below.</span>
                    </div>
                  ) : null}

                  {/* The requests this account can answer or is waiting on. Incoming rows carry
                      Accept and Decline (the wire's friendRespond); outgoing rows state what
                      they are — there is no unsend opcode to offer. */}
                  <FriendRequestsSection
                    incoming={incoming}
                    outgoing={outgoing}
                    profiles={profiles}
                    busy={socialBusy}
                    onAccept={(userId) => respond(userId, true)}
                    onDecline={(userId) => respond(userId, false)}
                  />

                  {/* The group chats are a Friends matter — the people are here — so the groups list
                      and its start control live in this tab, leaving Rooms to rooms. */}
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

                  {/* The graph's own suggestions close the view: this is the tab's discovery path,
                      and it sits under the lists so it never crowds them — a fresh account
                      without it would have nothing but exact-username search. */}
                  {suggestions.length > 0 ? (
                    <>
                      <div className="list-section-head">Suggestions ({suggestions.length})</div>
                      {suggestions.map((person) => (
                        <SuggestionRow
                          key={person.accountId}
                          person={person}
                          busy={socialBusy.has(person.accountId)}
                          onRequest={() => requestFriend(person.accountId)}
                        />
                      ))}
                    </>
                  ) : null}
                </>
              )}
            </>
          ) : null}

          {/* ===== ROOMS ===== */}
          {nav === 'rooms' ? (
            <>
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
                      <span>
                        {roomQuery.trim().length > 0
                          ? 'No rooms matched your search.'
                          : 'No public rooms on this server yet.'}
                      </span>
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
        {/* The list view maps to the band's "chats" hint; the other tabs map straight through. */}
        <ListFooter tab={nav === 'main' ? 'chats' : nav} hint={footerHint} />
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
          label="Settings"
          onClick={() => fromSheet(() => onOpenWindow('settings'))}
        />
        <SheetAction
          icon="shield"
          label="Security Checkup"
          sub="Identity · devices · backup · E2EE"
          onClick={() => fromSheet(() => onOpenWindow('checkup'))}
        />
        <SheetAction
          icon="shield"
          label="My Account"
          sub="Username · email · key file"
          onClick={() => fromSheet(() => onOpenWindow('account'))}
        />
        <SheetAction
          icon="wallet"
          label="My Wallet"
          sub="$MIG balance · gifts · AVAX"
          onClick={() => fromSheet(() => onOpenWindow('wallet'))}
        />
        <SheetAction
          icon="bell"
          label="Messages"
          sub={unreadTotal > 0 ? `${unreadTotal} unread` : undefined}
          onClick={() => fromSheet(() => onOpenWindow('notifications'))}
        />
        <SheetAction
          icon="phone"
          label="Calls"
          sub="Recent and missed calls"
          onClick={() => fromSheet(() => onOpenWindow('calls'))}
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
          icon="bot"
          label="Bots"
          sub="Accounts you run"
          onClick={() => fromSheet(() => onOpenWindow('bots'))}
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

      {/* New room dialog — the same Create Room flow the desktop's Rooms panel opens */}
      {roomDialogOpen ? (
        <CreateRoomDialog
          onOpenConversation={(conversationId) => {
            setRoomDialogOpen(false);
            onOpenConversation(conversationId);
          }}
          onClose={() => setRoomDialogOpen(false)}
        />
      ) : null}
    </div>
  );
}

/** The slice of a resolved profile the request and suggestion rows read. */
type PersonProfile = {
  displayName: string;
  username?: string;
  avatarUrl?: string;
  /** The bot behind this account, when there is one, so the row can say so. */
  botId?: Id;
};

/**
 * The Friends view's requests section: incoming requests with their answer, outgoing ones stated
 * for what they are.
 *
 * Exported presentational over plain data, so the section's rules — an incoming row carries
 * Accept and Decline wired to the wire's one respond call, an outgoing row offers nothing because
 * no opcode can recall it, and an empty graph renders nothing at all rather than an empty
 * heading — are testable without a live client, the same bargain the Friends panel's exported
 * sections make. The rows are divs, not buttons, because the incoming rows hold buttons of their
 * own.
 */
export function FriendRequestsSection({
  incoming,
  outgoing,
  profiles,
  busy,
  onAccept,
  onDecline,
}: {
  /** The requests waiting on this account's answer. */
  incoming: RelationshipEntry[];
  /** The requests this account has sent. */
  outgoing: RelationshipEntry[];
  /** Resolved profiles through the shared cache; an unresolved account keeps a stable fallback. */
  profiles: ReadonlyMap<Id, PersonProfile>;
  /** The ids with an answer in flight, so that row's buttons disable while it settles. */
  busy?: ReadonlySet<Id>;
  onAccept: (userId: Id) => void;
  onDecline: (userId: Id) => void;
}): ReactNode {
  if (incoming.length === 0 && outgoing.length === 0) {
    return null;
  }
  return (
    <>
      <div className="list-section-head">Requests ({incoming.length + outgoing.length})</div>
      {incoming.map((entry) => (
        <div key={entry.userId} className="mhome-row">
          <Avatar
            name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
            id={entry.userId}
            size={44}
            avatarUrl={profiles.get(entry.userId)?.avatarUrl}
          />
          <span className="mhome-row-main">
            <span className="mhome-row-name">
              {profiles.get(entry.userId)?.displayName ?? 'Someone'}
              <BotBadge botId={profiles.get(entry.userId)?.botId} compact />
            </span>
            <span className="mhome-row-sub">wants to be friends</span>
          </span>
          <span className="person-actions">
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={busy?.has(entry.userId) ?? false}
              onClick={() => onAccept(entry.userId)}
            >
              Accept
            </button>
            <button
              type="button"
              className="btn btn-ghost btn-sm"
              disabled={busy?.has(entry.userId) ?? false}
              onClick={() => onDecline(entry.userId)}
            >
              Decline
            </button>
          </span>
        </div>
      ))}
      {outgoing.map((entry) => (
        <div key={entry.userId} className="mhome-row">
          <Avatar
            name={profiles.get(entry.userId)?.displayName ?? 'Someone'}
            id={entry.userId}
            size={44}
            avatarUrl={profiles.get(entry.userId)?.avatarUrl}
          />
          <span className="mhome-row-main">
            <span className="mhome-row-name">
              {profiles.get(entry.userId)?.displayName ?? 'Someone'}
              <BotBadge botId={profiles.get(entry.userId)?.botId} compact />
            </span>
            <span className="mhome-row-sub">request sent</span>
          </span>
        </div>
      ))}
    </>
  );
}

/**
 * One suggested (or searched) person: who they are, the mutual friends that vouch for them, and
 * the one ask the wire carries — a friend request. Not a door to an intent sheet: the row's whole
 * business is the ask, and the sheet's own add-friend action would be a second way to say it.
 */
function SuggestionRow({
  person,
  busy,
  onRequest,
}: {
  person: SuggestedUser;
  busy: boolean;
  onRequest: () => void;
}): ReactNode {
  return (
    <div className="mhome-row">
      <Avatar name={person.displayName} id={person.accountId} size={44} />
      <span className="mhome-row-main">
        <span className="mhome-row-name">
          {person.displayName}
          <BotBadge botId={person.botId} compact />
        </span>
        <span className="mhome-row-sub">
          @{person.username}
          {person.mutualFriends > 0 ? ` · ${person.mutualFriends} mutual friends` : ''}
        </span>
      </span>
      <span className="person-actions">
        <button
          type="button"
          className="btn btn-primary btn-sm"
          disabled={busy}
          onClick={onRequest}
        >
          Add friend
        </button>
      </span>
    </div>
  );
}
