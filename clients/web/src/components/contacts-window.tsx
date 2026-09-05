'use client';

/**
 * The desktop's Contacts window.
 *
 * The reference design does not give the messenger a fixed sidebar: the account's lists live in a
 * window of their own — a frame with the gloss title bar, teal nav pills (Friends, Rooms, Feed),
 * the orange me bar (avatar, blinking presence dot, click-to-edit status, presence dropdown, the
 * mail chip, the away moon), a frosted toolbar, and the credit band at the foot. It can be
 * minimized (its taskbar button restores it), maximized, resized from its edges, and closed —
 * closing it is asking to log out, because with the contacts window gone there is no desk left to
 * come back to.
 *
 * The three bodies are the app's real panels, not restatements of them: the Friends panel (the
 * relationship graph, requests, suggestions, search, blocks), the Rooms panel (the directory), and
 * the Space panel (the activity stream). The me bar publishes real presence and status; the mail
 * chip is the Alerts window; the gear menu opens the real side windows.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { PointerEvent as ReactPointerEvent, ReactNode } from 'react';
import { createPortal } from 'react-dom';

import { ConversationKind, PresenceState } from '@migo/sdk';
import type { Id, PresenceState as PresenceStateValue } from '@migo/sdk';

import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useMePresence } from '@/lib/migo/use-me-presence.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Avatar } from './avatar.js';
import { FriendsPanel } from './friends-panel.js';
import { Icon } from './icons.js';
import { ListFooter } from './list-footer.js';
import { NewConversationDialog } from './new-conversation-dialog.js';
import { RoomsPanel } from './rooms-panel.js';
import { SpacePanel } from './space-panel.js';
import { presenceColor, presenceName } from './intent-sheet.js';
import type { WinKind } from './window-types.js';

/** The window's internal tabs — the reference's own three. */
type ContactsTab = 'friends' | 'rooms' | 'feed';

/** The self-reportable states, in display order. */
const PRESENCE_OPTIONS: ReadonlyArray<PresenceStateValue> = [
  PresenceState.Online,
  PresenceState.Busy,
  PresenceState.Away,
  PresenceState.Invisible,
];

/** The gear menu's everyday entries: a label, an icon, and the window each opens. */
const MENU_ENTRIES: ReadonlyArray<{
  kind: Exclude<WinKind, 'chat'>;
  label: string;
  icon: 'user' | 'settings' | 'wallet' | 'gift' | 'search' | 'game';
}> = [
  { kind: 'profile', label: 'My Profile', icon: 'user' },
  { kind: 'settings', label: 'Edit Profile & Settings', icon: 'settings' },
  { kind: 'wallet', label: 'My Credits & TopUp', icon: 'wallet' },
  { kind: 'search', label: 'Search', icon: 'search' },
  { kind: 'games', label: 'Games', icon: 'game' },
  { kind: 'store', label: 'Store', icon: 'gift' },
];

/** What the me bar's status edit accepts, matching the profile field's bound. */
const STATUS_MAX_CHARS = 100;

export function ContactsWindow({
  tab,
  onTabChange,
  width,
  height,
  maximized,
  onMinimize,
  onToggleMaximize,
  onClose,
  onResize,
  onOpenWindow,
  onOpenConversation,
  onMenuOpenChange,
}: {
  /** The active internal tab — held by the shell so section navigation can drive it. */
  tab: ContactsTab;
  onTabChange: (tab: ContactsTab) => void;
  width: number;
  height: number;
  maximized: boolean;
  onMinimize: () => void;
  onToggleMaximize: () => void;
  /** The close control's answer is the logout question, not a disappearance. */
  onClose: () => void;
  onResize: (w: number, h: number) => void;
  /** Opens one of the app's side windows — the toolbar's and the gear menu's action. */
  onOpenWindow: (kind: Exclude<WinKind, 'chat'>) => void;
  onOpenConversation: (conversationId: Id) => void;
  /** The portal menus lift the window above the desk while one is open. */
  onMenuOpenChange?: (open: boolean) => void;
}): ReactNode {
  const { client, accountId } = useMigo();
  const me = useMePresence();
  const { items } = useConversations();

  const [menuOpen, setMenuOpen] = useState(false);
  const [presOpen, setPresOpen] = useState(false);
  const [statusEditing, setStatusEditing] = useState(false);
  const [statusDraft, setStatusDraft] = useState('');
  const [owner, setOwner] = useState(false);
  const [groupDialogOpen, setGroupDialogOpen] = useState(false);

  const menuRef = useRef<HTMLDivElement | null>(null);
  const menuBtnRef = useRef<HTMLButtonElement | null>(null);
  const presBtnRef = useRef<HTMLButtonElement | null>(null);
  const [menuPos, setMenuPos] = useState({ left: 0, top: 0 });
  const [presPos, setPresPos] = useState({ left: 0, top: 0 });

  // The gear menu's owner-only entry: a server answer, not a build constant.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client
      .adminStanding()
      .then((standing) => {
        if (!cancelled) {
          setOwner(standing.owner);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client]);

  const setMenu = useCallback(
    (open: boolean): void => {
      if (open && menuBtnRef.current !== null) {
        const r = menuBtnRef.current.getBoundingClientRect();
        setMenuPos({ left: Math.max(4, r.right - 168), top: r.bottom + 2 });
      }
      setMenuOpen(open);
      onMenuOpenChange?.(open);
    },
    [onMenuOpenChange],
  );

  const openPresence = useCallback((open: boolean): void => {
    if (open && presBtnRef.current !== null) {
      const r = presBtnRef.current.getBoundingClientRect();
      setPresPos({ left: Math.max(4, r.right - 150), top: r.bottom + 4 });
    }
    setPresOpen(open);
  }, []);

  // A click outside either portal menu closes it; the menus are menus, not modes. The closers
  // are stable, so the listener is armed once per open state rather than once per render.
  const closeMenus = useCallback((): void => {
    setMenu(false);
    openPresence(false);
  }, [setMenu, openPresence]);

  useEffect(() => {
    if (!menuOpen && !presOpen) {
      return;
    }
    function onPointerDown(event: PointerEvent): void {
      const target = event.target as Node;
      const outsideMenus = menuRef.current === null || !menuRef.current.contains(target);
      const outsideButtons =
        (menuBtnRef.current === null || !menuBtnRef.current.contains(target)) &&
        (presBtnRef.current === null || !presBtnRef.current.contains(target));
      if (outsideMenus && outsideButtons) {
        closeMenus();
      }
    }
    document.addEventListener('pointerdown', onPointerDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
    };
  }, [menuOpen, presOpen, closeMenus]);

  function commitStatus(): void {
    me.publish(me.presence, statusDraft.trim());
    setStatusEditing(false);
  }

  // ---- resize (not while maximized) ----
  const resizeRef = useRef<{ sw: number; sh: number; sx: number; sy: number } | null>(null);
  function onResizeDown(event: ReactPointerEvent<HTMLDivElement>): void {
    event.preventDefault();
    resizeRef.current = { sw: width, sh: height, sx: event.clientX, sy: event.clientY };
    event.currentTarget.setPointerCapture(event.pointerId);
  }
  function onResizeMove(event: ReactPointerEvent<HTMLDivElement>): void {
    const r = resizeRef.current;
    if (r === null) {
      return;
    }
    const w = Math.min(Math.max(r.sw + (event.clientX - r.sx), 250), window.innerWidth - 24);
    const h = Math.min(Math.max(r.sh + (event.clientY - r.sy), 400), window.innerHeight - 108);
    onResize(w, h);
  }
  function onResizeUp(): void {
    resizeRef.current = null;
  }

  // The account's group chats, from the shared conversation list.
  const groups = items.filter((item) => item.kind === ConversationKind.Group);

  const rootStyle = maximized ? { width: '100%', height: '100%' } : { width, height };

  return (
    <div className="win-frame contacts-frame" style={rootStyle}>
      {/* Title bar */}
      <div className="gloss-title contacts-title">
        <span className="contacts-title-label">Contacts</span>
        <span className="win-controls">
          <button
            type="button"
            aria-label="Minimize"
            title="Minimize"
            className="win-ctl"
            onClick={onMinimize}
          >
            <Icon name="minimize" size={13} />
          </button>
          <button
            type="button"
            aria-label={maximized ? 'Restore' : 'Maximize'}
            title={maximized ? 'Restore' : 'Maximize'}
            className="win-ctl"
            onClick={onToggleMaximize}
          >
            <Icon name={maximized ? 'restore' : 'maximize'} size={13} />
          </button>
          <button
            type="button"
            aria-label="Close and log out"
            title="Close (log out)"
            className="win-ctl win-ctl-close"
            onClick={onClose}
          >
            <Icon name="close" size={13} />
          </button>
        </span>
      </div>

      {/* Teal nav — Friends / Rooms / Feed */}
      <div className="hdr-nav">
        {(
          [
            ['friends', 'Friends', 'friends'],
            ['rooms', 'Rooms', 'rooms'],
            ['feed', 'Feed', 'space'],
          ] as ReadonlyArray<[ContactsTab, string, 'friends' | 'rooms' | 'space']>
        ).map(([id, label, icon]) => (
          <button
            key={id}
            type="button"
            className={`hdr-pill${tab === id ? ' hdr-pill-active' : ''}`}
            onClick={() => onTabChange(id)}
          >
            <Icon name={icon} size={15} />
            {label}
          </button>
        ))}
      </div>

      {/* Orange me bar */}
      <div className="hdr-orange">
        <span className="me-avatar-ring">
          <Avatar name={me.displayName} id={accountId ?? 'me'} size={42} avatarUrl={me.avatarUrl} />
        </span>
        <div className="me-main">
          <div className="me-name-row">
            <span className="blink-dot" style={{ background: presenceColor(me.presence) }} />
            <span className="me-name">{me.displayName}</span>
          </div>
          {me.username.length > 0 ? <div className="me-handle">@{me.username}</div> : null}
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
              title="Click to edit your status"
            >
              {me.status.length > 0 ? me.status : 'New here! Say hi :)'}
            </button>
          )}
        </div>
        <div className="me-chips">
          <button
            ref={presBtnRef}
            type="button"
            className="hdr-chip"
            onClick={() => openPresence(!presOpen)}
            aria-label="Presence"
            title="Change your presence"
          >
            {presenceName(me.presence)}
            <Icon name={presOpen ? 'chevron-up' : 'chevron-down'} size={13} />{' '}
          </button>
          <button
            type="button"
            className="hdr-chip hdr-chip-icon"
            onClick={() => onOpenWindow('notifications')}
            title="Messages"
          >
            <Icon name="bell" size={13} />
          </button>
          <button
            type="button"
            className={`hdr-moon${me.presence === PresenceState.Away ? ' hdr-moon-on' : ''}`}
            onClick={() =>
              me.publish(
                me.presence === PresenceState.Away ? PresenceState.Online : PresenceState.Away,
                me.status,
              )
            }
            title={me.presence === PresenceState.Away ? 'Back to available' : 'Set away'}
            aria-label="Toggle away"
          >
            <Icon name="moon" size={15} />
          </button>
        </div>
      </div>

      {/* Toolbar */}
      <div className="gloss-panel contacts-toolbar">
        <button
          type="button"
          className="tbtn"
          onClick={() => onOpenWindow('search')}
          title="Search"
        >
          <Icon name="search" size={17} />
        </button>
        <button
          type="button"
          className="tbtn"
          onClick={() => onOpenWindow('notifications')}
          title="Messages"
        >
          <Icon name="bell" size={17} />
        </button>
        <button type="button" className="tbtn" onClick={() => onOpenWindow('games')} title="Games">
          <Icon name="game" size={17} />
        </button>
        <button type="button" className="tbtn" onClick={() => onOpenWindow('store')} title="Store">
          <Icon name="gift" size={17} />
        </button>
        <div className="toolbar-spacer" />
        <button
          ref={menuBtnRef}
          type="button"
          className={`tbtn tbtn-menu${menuOpen ? ' tbtn-on' : ''}`}
          onClick={() => setMenu(!menuOpen)}
          title="Menu"
        >
          <Icon name="settings" size={17} />
        </button>
      </div>

      {/* List body */}
      <div className="win-body retro-scroll contacts-body">
        {tab === 'friends' ? <FriendsPanel onOpenConversation={onOpenConversation} /> : null}
        {tab === 'rooms' ? (
          <>
            <div className="list-section-head list-section-head-row">
              <span>Your groups ({groups.length})</span>
              <button
                type="button"
                className="list-section-action"
                onClick={() => setGroupDialogOpen(true)}
                title="Create a new group chat with your friends"
              >
                <Icon name="user-plus" size={13} /> New group
              </button>
            </div>
            {groups.map((group) => (
              <button
                key={group.conversationId}
                type="button"
                className="list-row list-row-button"
                onClick={() => onOpenConversation(group.conversationId)}
                title="Open the group chat"
              >
                <span className="part-chip part-chip-group">
                  <Icon name="chats" size={20} />
                </span>
                <span className="list-row-main">
                  <span className="list-row-title">
                    <span className="list-row-name">{group.title ?? 'Group'}</span>
                    <span className="room-count">
                      <b>{group.members?.length ?? 1}</b> members
                    </span>
                  </span>
                  <span className="list-row-sub">Private group chat</span>
                </span>
                <Icon name="chevron-right" size={18} className="list-row-go" />
              </button>
            ))}
            {groups.length === 0 ? (
              <div className="list-hint">
                No groups yet — press <b>New group</b> to create one with your friends.
              </div>
            ) : null}
            <RoomsPanel onOpenConversation={onOpenConversation} />
          </>
        ) : null}
        {tab === 'feed' ? <SpacePanel onOpenConversation={onOpenConversation} /> : null}
      </div>

      {/* Footer credits */}
      <ListFooter tab={tab} />

      {/* presence dropdown, portalled so the window never clips it */}
      {presOpen
        ? createPortal(
            <div
              ref={menuRef}
              className="retro-menu retro-menu-presence"
              style={{ left: presPos.left, top: presPos.top }}
            >
              {PRESENCE_OPTIONS.map((state) => (
                <button
                  key={state}
                  type="button"
                  className="retro-menu-item"
                  onClick={() => {
                    me.publish(state, me.status);
                    openPresence(false);
                  }}
                >
                  <span className="retro-menu-dot" style={{ background: presenceColor(state) }} />
                  {presenceName(state)}
                  {me.presence === state ? (
                    <Icon name="check" size={14} className="retro-menu-check" />
                  ) : null}
                </button>
              ))}
            </div>,
            document.body,
          )
        : null}

      {/* gear dropdown, portalled */}
      {menuOpen
        ? createPortal(
            <div
              className="retro-menu retro-menu-gear"
              style={{ left: menuPos.left, top: menuPos.top }}
            >
              {MENU_ENTRIES.map((entry) => (
                <button
                  key={entry.kind}
                  type="button"
                  className="retro-menu-item"
                  onClick={() => {
                    setMenu(false);
                    onOpenWindow(entry.kind);
                  }}
                >
                  <Icon name={entry.icon} size={14} />
                  {entry.label}
                </button>
              ))}
              {owner ? (
                <button
                  type="button"
                  className="retro-menu-item"
                  onClick={() => {
                    setMenu(false);
                    onOpenWindow('admins');
                  }}
                >
                  <Icon name="shield" size={14} />
                  Global Admins
                </button>
              ) : null}
              <div className="retro-menu-sep" />
              <button
                type="button"
                className="retro-menu-item retro-menu-danger"
                onClick={() => {
                  setMenu(false);
                  onClose();
                }}
              >
                <Icon name="signout" size={14} />
                Logout
              </button>
            </div>,
            document.body,
          )
        : null}

      {/* New group dialog */}
      {groupDialogOpen ? <NewConversationDialog onClose={() => setGroupDialogOpen(false)} /> : null}

      {/* Resize handles */}
      {!maximized ? (
        <>
          <div
            className="rz-handle rz-e"
            onPointerDown={onResizeDown}
            onPointerMove={onResizeMove}
            onPointerUp={onResizeUp}
          />
          <div
            className="rz-handle rz-s"
            onPointerDown={onResizeDown}
            onPointerMove={onResizeMove}
            onPointerUp={onResizeUp}
          />
          <div
            className="rz-handle rz-se"
            onPointerDown={onResizeDown}
            onPointerMove={onResizeMove}
            onPointerUp={onResizeUp}
          />
        </>
      ) : null}
    </div>
  );
}
