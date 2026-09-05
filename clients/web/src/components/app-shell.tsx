'use client';

/**
 * The shell: a desk of windows on a PC, a tab strip on a phone.
 *
 * The reference design is a desktop-OS metaphor, and this module is its window manager. On a PC
 * the turquoise desk carries the Contacts window (the account's lists — Friends, Rooms, Feed — in
 * a draggable, resizable, minimizable frame of their own) and every conversation or panel the
 * account opens lands beside it as its own RetroWindow, cascaded, draggable, resizable, with a
 * z-order the last click wins. The taskbar at the desk's edge (which edge is a stored choice)
 * holds one button per window, the $MIG balance, the session timer, and the clock.
 *
 * Below the PC breakpoint the desk disappears: a 46px tab strip holds the home tabs (Friends,
 * Rooms, Feed — only Feed closable) and one tab per open window, and the window the strip selects
 * renders full-bleed below it. People and rooms become intent sheets rather than double-clicks,
 * because a thumb has no hover.
 *
 * The URL fragment stays the single source of truth for the open conversation (see
 * use-open-conversation.ts): every door into a thread is `openConversation(id)`, and the fragment
 * effect below opens or focuses that conversation's window — and takes it away again when the
 * fragment clears, leaving rooms and groups on the way out exactly as the previous shell did.
 * A message that arrives for a conversation with no window mints one (the session's own rule),
 * without stealing focus from whoever has it; the shell's own unread counts (not the provider's,
 * which a mounted thread's mark-read clears) drive the badges.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { CallMediaKind, ConversationKind } from '@migo/sdk';
import type { ConversationSummary, Id, RoomSummary } from '@migo/sdk';

import { AccountPanel } from './account-panel.js';
import { AdminsPanel } from './admins-panel.js';
import { ChatWindow } from './chat-window.js';
import { ConfirmDialog } from './confirm-dialog.js';
import { ContactsWindow } from './contacts-window.js';
import { GamesPanel } from './games-panel.js';
import { Icon } from './icons.js';
import { MobileHome } from './mobile-home.js';
import { MobileTabBar } from './mobile-tab-bar.js';
import type { MobileNavTab } from './mobile-tab-bar.js';
import { MOBILE_NAV_ORDER } from './mobile-tab-bar.js';
import { NotificationsPanel } from './notifications-panel.js';
import { ProfilePanel } from './profile-panel.js';
import { RetroWindow } from './retro-window.js';
import { SearchPanel } from './search-panel.js';
import { SettingsPanel } from './settings-panel.js';
import { StorePanel } from './store-panel.js';
import { Taskbar } from './desktop-taskbar.js';
import { UserIntentSheet, RoomIntentSheet } from './intent-sheet.js';
import { WalletPanel } from './wallet-panel.js';
import { MigoBrand } from './migo-brand.js';
import { chatWinId } from './window-types.js';
import type { WinKind, WinState } from './window-types.js';
import { KIND_LABEL, STORE_WINDOW, WINDOW_SIZES } from './window-types.js';

import { useCall } from '@/lib/migo/call-manager.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import { useJoinRoom } from '@/lib/migo/use-join-room.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import {
  closeConversation,
  openConversation,
  useOpenConversation,
} from '@/lib/migo/use-open-conversation.js';
import { usePresenceOf } from '@/lib/migo/use-presence.js';
import { useProfile, useProfiles } from '@/lib/migo/use-profiles.js';
import type { ResolvedProfile } from '@/lib/migo/use-profiles.js';
import { useRooms } from '@/lib/migo/rooms-provider.js';
import type { RoomsContextValue } from '@/lib/migo/rooms-provider.js';
import { SectionNavProvider } from '@/lib/migo/section-nav.js';

/** The sections a deep surface can ask the shell to navigate to. */
export type AppTab = 'friends' | 'chats' | 'rooms' | 'feed' | Exclude<WinKind, 'chat'>;

/** The breakpoint below which the desk becomes a phone. */
const MOBILE_BREAKPOINT = 768;

/** Where the taskbar position preference lives (the design's own key). */
const TASKBAR_POS_KEY = 'migo.taskbarPos';

/** The chat window's opening size: a thread wants more room than a settings panel. */
const CHAT_WINDOW = { w: 520, h: 460 };

/** The window sizes per kind: the default side window, the store's own, and a chat's. */
function sizeForKind(kind: WinKind): { w: number; h: number } {
  if (kind === 'chat') {
    return CHAT_WINDOW;
  }
  if (kind === 'store') {
    return STORE_WINDOW;
  }
  return WINDOW_SIZES;
}

/**
 * The shell itself. It must sit inside the conversations, rooms, muted, and call providers (see
 * app/chat/layout.tsx) — it is their reader — and it provides the section navigation the threads
 * below it use.
 */
export function AppShell(): ReactNode {
  const { client, accountId, logout } = useMigo();
  const openId = useOpenConversation();
  const { items, noteConversation } = useConversations();
  const rooms = useRooms();
  const { startCall } = useCall();

  // ---- the desk's own state ----
  const [mounted, setMounted] = useState(false);
  const [isMobile, setIsMobile] = useState(false);
  const [taskbarPos, setTaskbarPos] = useState<'bottom' | 'top'>('bottom');
  const [onlineSince, setOnlineSince] = useState(0);

  const [windows, setWindows] = useState<WinState[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [unreadWin, setUnreadWin] = useState<Record<string, number>>({});

  const [contactsTab, setContactsTab] = useState<'friends' | 'rooms' | 'feed'>('friends');
  const [contactsSize, setContactsSize] = useState({ w: 360, h: 560 });
  const [contactsMax, setContactsMax] = useState(false);
  const [contactsMin, setContactsMin] = useState(false);
  const [contactsMenuOpen, setContactsMenuOpen] = useState(false);

  const [confirmLogout, setConfirmLogout] = useState(false);

  // ---- the phone's own state ----
  const [mobileNav, setMobileNav] = useState<MobileNavTab>('feed');
  const [hiddenNavs, setHiddenNavs] = useState<MobileNavTab[]>([]);
  const [intentUser, setIntentUser] = useState<Id | null>(null);
  const [intentRoom, setIntentRoom] = useState<RoomSummary | null>(null);

  const zRef = useRef(20);
  const cascadeRef = useRef(0);
  const prevOpenRef = useRef<Id | null>(null);
  // The live window-id and focus sets, as refs so the message handler below reads the latest
  // without re-subscribing.
  const winIdsRef = useRef<ReadonlySet<string>>(new Set());
  const activeIdRef = useRef<string | null>(null);

  // Mount guard (no SSR randomness), the breakpoint, the taskbar preference, the session clock.
  useEffect(() => {
    const t = setTimeout(() => {
      setMounted(true);
      setIsMobile(window.innerWidth < MOBILE_BREAKPOINT);
      setOnlineSince(Date.now());
      try {
        const saved = window.localStorage.getItem(TASKBAR_POS_KEY);
        if (saved === 'top' || saved === 'bottom') {
          setTaskbarPos(saved);
        }
      } catch {
        /* storage unavailable */
      }
    }, 0);
    const onResize = (): void => setIsMobile(window.innerWidth < MOBILE_BREAKPOINT);
    window.addEventListener('resize', onResize);
    return () => {
      clearTimeout(t);
      window.removeEventListener('resize', onResize);
    };
  }, []);

  const toggleTaskbarPos = useCallback((): void => {
    setTaskbarPos((prev) => {
      const next = prev === 'bottom' ? 'top' : 'bottom';
      try {
        window.localStorage.setItem(TASKBAR_POS_KEY, next);
      } catch {
        /* storage unavailable */
      }
      return next;
    });
  }, []);

  // ---- window operations ----
  const focusWin = useCallback((id: string): void => {
    zRef.current += 1;
    const z = zRef.current;
    setWindows((ws) => ws.map((w) => (w.id === id ? { ...w, z, minimized: false } : w)));
    setActiveId(id);
    setUnreadWin((u) => ({ ...u, [id]: 0 }));
  }, []);

  /** Opens (or brings forward) a window; on a phone every other window is parked. */
  const openWindow = useCallback(
    (w: Omit<WinState, 'x' | 'y' | 'z' | 'minimized'>): void => {
      setWindows((ws) => {
        const existing = ws.find((x) => x.id === w.id) !== undefined;
        zRef.current += 1;
        const z = zRef.current;
        if (existing) {
          return ws.map((x) =>
            x.id === w.id
              ? { ...x, z, minimized: false }
              : isMobile
                ? { ...x, minimized: true }
                : x,
          );
        }
        const step = (cascadeRef.current = (cascadeRef.current + 1) % 8);
        const bx = isMobile ? 0 : 270 + step * 26;
        const by = isMobile ? 0 : 30 + step * 24;
        return [
          ...ws.map((x) => (isMobile ? { ...x, minimized: true } : x)),
          { ...w, x: bx, y: by, z, minimized: false },
        ];
      });
      setActiveId(w.id);
      setUnreadWin((u) => ({ ...u, [w.id]: 0 }));
    },
    [isMobile],
  );

  const minimizeWindow = useCallback((id: string): void => {
    setWindows((ws) => ws.map((w) => (w.id === id ? { ...w, minimized: true } : w)));
    setActiveId(null);
  }, []);

  const moveWindow = useCallback((id: string, x: number, y: number): void => {
    setWindows((ws) => ws.map((w) => (w.id === id ? { ...w, x, y } : w)));
  }, []);

  /**
   * Leaves the conversation a closed chat window held, when leaving is what closing means.
   *
   * Best-effort by design: the window is gone either way, and a leave the server refused (offline,
   * already left) must not resurrect it.
   */
  const leaveWhenClosed = useCallback(
    (conversationId: Id): void => {
      if (client === null) {
        return;
      }
      const summary = items.find((item) => item.conversationId === conversationId);
      if (summary === undefined) {
        return;
      }
      if (summary.kind === ConversationKind.Room) {
        const room = rooms.infoFor(conversationId);
        if (room !== null) {
          void client.rooms
            .leave(room.roomId)
            .then(() => {
              // The leaver's own device is excluded from the member fan-out, so nothing on the
              // wire corrects the held record — drop it here.
              rooms.forgetRoom(room.roomId);
            })
            .catch(() => undefined);
        }
        return;
      }
      if (summary.kind === ConversationKind.Group) {
        void client.conversations.leave(conversationId).catch(() => undefined);
      }
    },
    [client, items, rooms],
  );

  const closeWindow = useCallback(
    (id: string): void => {
      const window = windows.find((w) => w.id === id);
      setWindows((ws) => ws.filter((w) => w.id !== id));
      setUnreadWin((u) => ({ ...u, [id]: 0 }));
      setActiveId((cur) => (cur === id ? null : cur));
      if (window?.kind === 'chat' && window.conversationId !== undefined) {
        // Closing a room or group window is walking out of it; a direct chat is not membership —
        // closing it is the whole goodbye.
        leaveWhenClosed(window.conversationId);
        if (window.conversationId === openId) {
          closeConversation();
        }
      }
    },
    [leaveWhenClosed, openId, windows],
  );

  /** The taskbar button's one-click cycle: restore, focus, or minimize. */
  const toggleWin = useCallback(
    (id: string): void => {
      const w = windows.find((x) => x.id === id);
      if (w === undefined) {
        return;
      }
      if (w.minimized || w.id !== activeId) {
        focusWin(id);
        return;
      }
      minimizeWindow(id);
    },
    [activeId, focusWin, minimizeWindow, windows],
  );

  // ---- the fragment: the open conversation's window ----
  useEffect(() => {
    const prev = prevOpenRef.current;
    prevOpenRef.current = openId;
    if (openId === prev) {
      // Not a change — a re-run because the lists moved. Nothing to reconcile.
      return;
    }
    if (openId !== null) {
      openWindow({ id: chatWinId(openId), kind: 'chat', conversationId: openId, title: '' });
      return;
    }
    if (prev !== null) {
      // Back cleared the fragment: the thread's window goes with it, and the leave it owes (a
      // room, a group) is paid on the way out. A window the close button already took — its own
      // close path paid the leave and cleared the fragment — is no longer here to pay twice.
      const gone = chatWinId(prev);
      if (winIdsRef.current.has(gone)) {
        leaveWhenClosed(prev);
        setWindows((ws) => ws.filter((w) => w.id !== gone));
        setUnreadWin((u) => ({ ...u, [gone]: 0 }));
        setActiveId((cur) => (cur === gone ? null : cur));
      }
    }
  }, [openId, openWindow, leaveWhenClosed]);

  // ---- message arrival: mint windows, count attention ----
  winIdsRef.current = new Set(windows.map((w) => w.id));
  activeIdRef.current = activeId;
  useEffect(() => {
    if (client === null || accountId === null) {
      return;
    }
    const off = client.messaging.onMessage((message) => {
      if (message.senderId === accountId) {
        // Echoes of this account's own sends: the sending device already opened its window.
        return;
      }
      const id = chatWinId(message.conversationId);
      if (winIdsRef.current.has(id)) {
        // The window exists: count the message unless the person is looking at it.
        if (id !== activeIdRef.current) {
          setUnreadWin((u) => ({ ...u, [id]: (u[id] ?? 0) + 1 }));
        }
        return;
      }
      // No window yet: mint one (the session's rule — a packet brings its window into being)
      // without stealing focus from whoever has it.
      zRef.current += 1;
      const z = zRef.current;
      const step = (cascadeRef.current = (cascadeRef.current + 1) % 8);
      setWindows((ws) => {
        if (ws.some((w) => w.id === id)) {
          return ws;
        }
        return [
          ...ws.map((x) => (isMobile ? { ...x, minimized: true } : x)),
          {
            id,
            kind: 'chat' as const,
            conversationId: message.conversationId,
            title: '',
            x: isMobile ? 0 : 270 + step * 26,
            y: isMobile ? 0 : 30 + step * 24,
            z,
            minimized: isMobile,
          },
        ];
      });
      setUnreadWin((u) => ({ ...u, [id]: 1 }));
    });
    return () => {
      off();
    };
  }, [client, accountId, isMobile]);

  // ---- the phone's navigation ----
  /** Parks every window and returns to the home screen. */
  const goHome = useCallback((): void => {
    setWindows((ws) => ws.map((w) => (!w.minimized ? { ...w, minimized: true } : w)));
    setActiveId(null);
  }, []);

  const selectMobileNav = useCallback(
    (tab: MobileNavTab): void => {
      setMobileNav(tab);
      goHome();
    },
    [goHome],
  );

  /** A window tab on the phone: park the others, bring this one forward. */
  const mobileSelectWin = useCallback(
    (id: string): void => {
      setWindows((ws) =>
        ws.map((w) => (w.id !== id && !w.minimized ? { ...w, minimized: true } : w)),
      );
      focusWin(id);
    },
    [focusWin],
  );

  const closeMobileNav = useCallback(
    (tab: MobileNavTab): void => {
      setHiddenNavs((hs) => (hs.includes(tab) ? hs : [...hs, tab]));
      setMobileNav((cur) => {
        if (cur !== tab) {
          return cur;
        }
        return MOBILE_NAV_ORDER.find((x) => x !== tab && !hiddenNavs.includes(x)) ?? cur;
      });
    },
    [hiddenNavs],
  );

  const reopenMobileNav = useCallback(
    (tab: MobileNavTab): void => {
      setHiddenNavs((hs) => hs.filter((x) => x !== tab));
      setMobileNav(tab);
      goHome();
    },
    [goHome],
  );

  // ---- opening things ----
  /** Opens (or focuses) a conversation's window: the one door every surface uses. */
  const openChat = useCallback(
    (conversationId: Id): void => {
      if (openId === conversationId) {
        focusWin(chatWinId(conversationId));
        return;
      }
      openConversation(conversationId);
    },
    [focusWin, openId],
  );

  /** Opens (or focuses) one of the app's side windows. */
  const openPanelWindow = useCallback(
    (kind: Exclude<WinKind, 'chat'>): void => {
      openWindow({ id: kind, kind, title: KIND_LABEL[kind] });
    },
    [openWindow],
  );

  /** The direct conversation with a person, found or created — the intent sheets' door. */
  const ensureDirect = useCallback(
    async (userId: Id): Promise<Id | null> => {
      if (client === null) {
        return null;
      }
      const existing = items.find(
        (item) => item.kind === ConversationKind.Direct && (item.members ?? []).includes(userId),
      );
      if (existing !== undefined) {
        return existing.conversationId;
      }
      try {
        const summary = await client.conversations.create(ConversationKind.Direct, [userId]);
        noteConversation(summary);
        return summary.conversationId;
      } catch {
        return null;
      }
    },
    [client, items, noteConversation],
  );

  const { join } = useJoinRoom(openChat);

  // The cross-section navigation the deep surfaces share: the home tabs drive the contacts
  // window (or the phone's home), the panels arrive as windows.
  const navigate = useCallback(
    (tab: AppTab): void => {
      if (tab === 'friends' || tab === 'rooms' || tab === 'feed') {
        setContactsTab(tab);
        if (isMobile) {
          selectMobileNav(tab);
        }
        return;
      }
      if (tab === 'chats') {
        // No chats list survives the window metaphor; the friends list is where a person is.
        setContactsTab('friends');
        if (isMobile) {
          selectMobileNav('friends');
        }
        return;
      }
      openPanelWindow(tab);
    },
    [isMobile, openPanelWindow, selectMobileNav],
  );

  // ---- titles ----
  // A chat window's title is derived per render — the room's #name, the peer's display name —
  // from the same sources the conversation rows read.
  const peerIds = useMemo(
    () =>
      [
        ...new Set(
          windows.flatMap((w) => {
            if (w.kind !== 'chat' || w.conversationId === undefined) {
              return [];
            }
            const summary = items.find((item) => item.conversationId === w.conversationId);
            if (summary?.kind !== ConversationKind.Direct) {
              return [];
            }
            return summary.members ?? [];
          }),
        ),
      ].filter((id) => id !== accountId),
    [windows, items, accountId],
  );
  const profiles = useProfiles(peerIds);

  const titleOf = useCallback(
    (w: WinState): string => {
      if (w.kind !== 'chat' || w.conversationId === undefined) {
        return w.title;
      }
      return chatTitleOf(w.conversationId, items, rooms, accountId, profiles);
    },
    [items, rooms, accountId, profiles],
  );

  const titledWindows = useMemo(
    () => windows.map((w) => ({ ...w, title: titleOf(w) })),
    [windows, titleOf],
  );

  // The home tabs' unread badges: the shell's own attention counts, by where the conversation
  // belongs. Feed is activity, not messages — its badge stays empty.
  const navUnread = useMemo(() => {
    const counts: Record<MobileNavTab, number> = { friends: 0, rooms: 0, feed: 0 };
    for (const w of windows) {
      if (w.kind !== 'chat' || w.conversationId === undefined) {
        continue;
      }
      const unread = unreadWin[w.id] ?? 0;
      if (unread === 0) {
        continue;
      }
      const summary = items.find((item) => item.conversationId === w.conversationId);
      if (summary?.kind === ConversationKind.Direct) {
        counts.friends += unread;
      } else {
        counts.rooms += unread;
      }
    }
    return counts;
  }, [windows, unreadWin, items]);

  // The account's own profile, for the taskbar's logout title.
  const self = useProfile(accountId);

  // The intent sheets' person: profile and live presence for whoever the list named.
  const intentProfile = useProfile(intentUser);
  const intentProfileMap = useMemo(
    () =>
      intentUser !== null && intentProfile !== null
        ? new Map<Id, ResolvedProfile>([[intentUser, intentProfile]])
        : undefined,
    [intentUser, intentProfile],
  );
  const intentPresenceMap = usePresenceOf(
    intentUser !== null ? [intentUser] : [],
    intentProfileMap,
  );
  const intentPresence = intentUser !== null ? intentPresenceMap.get(intentUser) : undefined;

  // The intent sheet's room, when one is open: the live record's counts over the snapshot's.
  const intentLive = intentRoom !== null ? rooms.liveFor(intentRoom.roomId) : null;

  // ---- logout ----
  const handleLogout = useCallback((): void => {
    setConfirmLogout(false);
    void logout();
  }, [logout]);

  // ---- what a window holds ----
  const windowContent = (w: WinState): ReactNode => {
    if (w.kind === 'chat') {
      return w.conversationId !== undefined ? (
        <ChatWindow conversationId={w.conversationId} />
      ) : null;
    }
    switch (w.kind) {
      case 'notifications':
        return <NotificationsPanel />;
      case 'search':
        return <SearchPanel onOpenConversation={openChat} />;
      case 'wallet':
        return <WalletPanel />;
      case 'profile':
        return <ProfilePanel onOpenSettings={() => openPanelWindow('settings')} />;
      case 'account':
        return <AccountPanel />;
      case 'settings':
        return <SettingsPanel />;
      case 'admins':
        return <AdminsPanel />;
      case 'store':
        return <StorePanel />;
      case 'games':
        return <GamesPanel />;
    }
  };

  // ---- the render ----
  if (!mounted) {
    return (
      <div className="desk-bg desk-boot">
        <span className="desk-boot-brand">
          <MigoBrand size={24} />
        </span>
      </div>
    );
  }

  const visibleNavs = MOBILE_NAV_ORDER.filter((tab) => !hiddenNavs.includes(tab));

  return (
    <SectionNavProvider navigate={navigate}>
      <div className="desk-bg desk-root">
        {/* watermark brand */}
        {!isMobile ? (
          <div
            className={`desk-mark${taskbarPos === 'top' ? ' desk-mark-top' : ''}`}
            aria-hidden="true"
          >
            <MigoBrand size={24} />
          </div>
        ) : null}

        {/* ===== the phone's home ===== */}
        {isMobile && visibleNavs.length > 0 ? (
          <div className={activeId !== null ? 'desk-home-hidden' : 'desk-home'}>
            <MobileHome
              nav={mobileNav}
              onOpenConversation={openChat}
              onOpenWindow={openPanelWindow}
              onOpenUserIntent={(userId) => setIntentUser(userId)}
              onOpenRoomIntent={(room) => setIntentRoom(room)}
              onRequestLogout={() => setConfirmLogout(true)}
            />
          </div>
        ) : null}

        {/* every home tab closed: the friendly empty state with one-tap reopens */}
        {isMobile && visibleNavs.length === 0 && activeId === null ? (
          <div className="mhome mhome-empty-state">
            <div className="mhome-empty-card">
              <Icon name="chats" size={34} />
              <div className="mhome-empty-title">No home tab open</div>
              <p className="mhome-empty-text">
                Feed, Friends and Rooms were closed from their X. Reopen a tab to continue — open
                chat windows stay in the strip above.
              </p>
              <div className="mhome-empty-actions">
                {MOBILE_NAV_ORDER.map((tab) => (
                  <button
                    key={tab}
                    type="button"
                    className="gloss-pill"
                    onClick={() => reopenMobileNav(tab)}
                  >
                    {tab === 'friends' ? 'Friends' : tab === 'rooms' ? 'Rooms' : 'Feed'}
                  </button>
                ))}
              </div>
            </div>
          </div>
        ) : null}

        {/* ===== the PC's contacts window ===== */}
        {!isMobile && !contactsMin ? (
          <div
            className="contacts-holder"
            style={
              contactsMax
                ? taskbarPos === 'top'
                  ? { position: 'fixed', left: 0, top: 34, right: 0, bottom: 0, zIndex: 950 }
                  : { position: 'fixed', left: 0, top: 0, right: 0, bottom: 34, zIndex: 950 }
                : { position: 'absolute', left: 12, top: 64, zIndex: contactsMenuOpen ? 1200 : 10 }
            }
          >
            <ContactsWindow
              tab={contactsTab}
              onTabChange={setContactsTab}
              width={contactsSize.w}
              height={contactsSize.h}
              maximized={contactsMax}
              onMinimize={() => setContactsMin(true)}
              onToggleMaximize={() => setContactsMax((v) => !v)}
              onClose={() => setConfirmLogout(true)}
              onResize={(w, h) => setContactsSize({ w, h })}
              onOpenWindow={openPanelWindow}
              onOpenConversation={openChat}
              onMenuOpenChange={setContactsMenuOpen}
            />
          </div>
        ) : null}

        {/* restore button for a minimized contacts window (the desk only) */}
        {!isMobile && contactsMin ? (
          <button
            type="button"
            className={`task-btn contacts-restore${taskbarPos === 'top' ? ' contacts-restore-top' : ''}`}
            onClick={() => setContactsMin(false)}
          >
            <span className="task-dot" aria-hidden="true" />
            Contacts
          </button>
        ) : null}

        {/* ===== the windows ===== */}
        {windows.map((w) => {
          if (isMobile && (w.id !== activeId || w.minimized)) {
            return null;
          }
          const size = sizeForKind(w.kind);
          return (
            <RetroWindow
              key={w.id}
              title={titleOf(w)}
              x={w.x}
              y={w.y}
              z={w.z}
              active={w.id === activeId && !w.minimized}
              width={size.w}
              height={size.h}
              minimized={w.minimized}
              mobileFullscreen={isMobile}
              taskbarPos={taskbarPos}
              onFocus={() => focusWin(w.id)}
              onMinimize={() => minimizeWindow(w.id)}
              onClose={() => closeWindow(w.id)}
              onMove={(x, y) => moveWindow(w.id, x, y)}
            >
              {windowContent(w)}
            </RetroWindow>
          );
        })}

        {/* ===== the strip or the taskbar ===== */}
        {isMobile ? (
          <MobileTabBar
            windows={titledWindows}
            activeId={activeId}
            unreadWin={unreadWin}
            navTab={mobileNav}
            hiddenNavs={hiddenNavs}
            navUnread={navUnread}
            onSelectNav={selectMobileNav}
            onCloseNav={closeMobileNav}
            onReopenNav={reopenMobileNav}
            onSelectWindow={mobileSelectWin}
            onCloseWindow={closeWindow}
          />
        ) : (
          <Taskbar
            windows={titledWindows}
            activeId={activeId}
            onlineSince={onlineSince}
            onToggle={toggleWin}
            onRequestLogout={() => setConfirmLogout(true)}
            accountName={self?.displayName ?? 'this account'}
            pos={taskbarPos}
            onTogglePos={toggleTaskbarPos}
          />
        )}

        {/* logout confirmation */}
        <ConfirmDialog
          open={confirmLogout}
          title="Log out of Migo?"
          message="You will be disconnected from all chats. Your key file stays on this device and you can sign in again any time."
          confirmLabel="Log out"
          onConfirm={handleLogout}
          onCancel={() => setConfirmLogout(false)}
        />

        {/* the phone's intent sheets — one tap on a person or room offers the actions */}
        {isMobile ? (
          <>
            <UserIntentSheet
              target={intentUser}
              name={intentProfile?.displayName ?? intentProfile?.username ?? 'Migo member'}
              username={intentProfile?.username}
              status={intentProfile?.customStatus}
              presence={intentPresence}
              avatarUrl={intentProfile?.avatarUrl}
              isFriend
              onClose={() => setIntentUser(null)}
              onSend={() => {
                const userId = intentUser;
                setIntentUser(null);
                if (userId !== null) {
                  void ensureDirect(userId).then((conversationId) => {
                    if (conversationId !== null) {
                      openChat(conversationId);
                    }
                  });
                }
              }}
              onCall={(video) => {
                const userId = intentUser;
                setIntentUser(null);
                if (userId !== null) {
                  void ensureDirect(userId).then((conversationId) => {
                    if (conversationId !== null) {
                      void startCall(
                        conversationId,
                        userId,
                        video ? CallMediaKind.Video : CallMediaKind.Audio,
                      );
                    }
                  });
                }
              }}
            />
            <RoomIntentSheet
              room={intentRoom}
              online={
                intentRoom !== null
                  ? (intentLive?.onlineCount ?? intentRoom.onlineCount)
                  : undefined
              }
              capacity={intentRoom?.maxMembers ?? intentLive?.maxMembers}
              joined={
                intentRoom !== null &&
                items.some(
                  (item) => rooms.infoFor(item.conversationId)?.roomId === intentRoom.roomId,
                )
              }
              onClose={() => setIntentRoom(null)}
              onJoin={() => {
                const room = intentRoom;
                setIntentRoom(null);
                if (room !== null) {
                  void join(room);
                }
              }}
            />
          </>
        ) : null}
      </div>
    </SectionNavProvider>
  );
}

/**
 * A chat window's title: the room's `#name` when the shell knows the room, the peer's display
 * name for a direct chat, and the summary's own title as the honest fallback.
 */
function chatTitleOf(
  conversationId: Id,
  items: ConversationSummary[],
  rooms: RoomsContextValue,
  accountId: Id | null,
  profiles: Map<Id, ResolvedProfile>,
): string {
  const summary = items.find((item) => item.conversationId === conversationId);
  if (summary === undefined) {
    return 'Chat';
  }
  if (summary.kind === ConversationKind.Direct) {
    const other = summary.members?.find((member) => member !== accountId) ?? null;
    return (other !== null ? profiles.get(other)?.displayName : undefined) ?? 'Direct message';
  }
  const room = rooms.infoFor(conversationId);
  return `#${room?.name ?? summary.title ?? 'room'}`;
}
