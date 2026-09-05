/**
 * What the window shell offers as the app's navigation.
 *
 * The messenger stopped being two panes and became a desk: every conversation and panel is a
 * window of its own (draggable, resizable, minimizable), the contacts list is a window too
 * rather than a fixed sidebar, and the taskbar — or, below the PC breakpoint, the tab strip —
 * is the one inventory of what is open. The tests pin that offer at each layer:
 *
 *   1. **The shell's boot state.** Before the mount guard lifts, the desk shows only its
 *      turquoise ground and the brand — no taskbar, no windows, nothing to mis-click.
 *   2. **The window chrome.** A desk window names itself in a teal title bar with min/max/close
 *      controls and resize handles; a phone window has none of it, because the strip's tab
 *      already names it and closes it; a minimized window renders nothing at all.
 *   3. **The taskbar.** One button per window, the focused one marked, a minimized window's
 *      button kept with the pale dot, the balance chip silent until the wallet answers, and the
 *      dock toggle that names the edge it will move to.
 *   4. **The phone's strip.** Friends, Rooms, Feed in the reference order with only Feed
 *      closable; the "+" that reopens a closed tab; one closable tab per window with its unread
 *      badge capped at "9+".
 *   5. **The vocabulary.** Every window kind has a label and an icon, and a chat window's id is
 *      its conversation's, so a thread can never open twice.
 *   6. **The contacts window.** A titled, pill-navigated window whose close control asks to log
 *      out — with the contacts window gone there is no desk left to come back to.
 *
 * `renderToStaticMarkup` runs no effects, so the shells that read providers are fed the same
 * provider stack the layout mounts, over a ready-session context double whose client is null —
 * exactly the "connected, nothing fetched yet" moment every session really passes through.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { AppShell } from '../src/components/app-shell.js';
import { ContactsWindow } from '../src/components/contacts-window.js';
import { MobileTabBar, MOBILE_NAV_ORDER } from '../src/components/mobile-tab-bar.js';
import type { MobileNavTab } from '../src/components/mobile-tab-bar.js';
import { RetroWindow } from '../src/components/retro-window.js';
import { Taskbar } from '../src/components/desktop-taskbar.js';
import {
  chatWinId,
  KIND_ICON,
  KIND_LABEL,
  STORE_WINDOW,
  WINDOW_SIZES,
} from '../src/components/window-types.js';
import type { WinKind, WinState } from '../src/components/window-types.js';
import { CallManagerProvider } from '../src/lib/migo/call-manager.js';
import { ConversationsProvider } from '../src/lib/migo/conversations-provider.js';
import { MutedProvider } from '../src/lib/migo/muted-provider.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import type { MigoContextValue } from '../src/lib/migo/provider.js';
import { RoomsProvider } from '../src/lib/migo/rooms-provider.js';

const ME = 'acct_self' as Id;

/** The ready-session context double: connected, but with nothing fetched yet. */
const CONTEXT: MigoContextValue = {
  status: 'ready',
  connectionState: 'ready',
  accountId: ME,
  deviceId: null,
  error: null,
  resetNonce: 0,
  persistKeyStore: () => {},
  client: null,
  register: () => Promise.resolve(),
  loginWithFile: () => Promise.resolve(),
  logout: () => Promise.resolve(),
};

/** The provider stack the layout mounts, over the context double. */
function sessionShell(node: ReactNode): string {
  return renderToStaticMarkup(
    <MigoContext.Provider value={CONTEXT}>
      <ConversationsProvider>
        <RoomsProvider>
          <MutedProvider>
            <CallManagerProvider>{node}</CallManagerProvider>
          </MutedProvider>
        </RoomsProvider>
      </ConversationsProvider>
    </MigoContext.Provider>,
  );
}

/** The context double alone — for the chrome that reads only the session, not the lists. */
function desk(node: ReactNode): string {
  return renderToStaticMarkup(<MigoContext.Provider value={CONTEXT}>{node}</MigoContext.Provider>);
}

/** One window on the desk. */
function win(fields: Partial<WinState> & { id: string; kind: WinKind; title: string }): WinState {
  return { x: 40, y: 60, z: 21, minimized: false, ...fields };
}

const NOOP = () => {};

// --- the shell's boot state ---

test('the shell boots onto an empty desk: ground and brand, nothing to mis-click', () => {
  const markup = sessionShell(<AppShell />);

  assert.ok(markup.includes('desk-boot'), 'the boot state must paint the desk ground');
  assert.ok(markup.includes('migo-brand'), 'the boot state must carry the brand');
  assert.ok(!markup.includes('taskbar'), 'no taskbar may exist before the desk mounts');
  assert.ok(!markup.includes('win-frame'), 'no window may exist before the desk mounts');
});

// --- the window chrome ---

test('a desk window names itself and carries min, max, close, and resize handles', () => {
  const markup = renderToStaticMarkup(
    <RetroWindow
      title="reason008"
      x={40}
      y={60}
      z={21}
      active
      width={520}
      height={460}
      onFocus={NOOP}
      onMinimize={NOOP}
      onClose={NOOP}
      onMove={NOOP}
    >
      <p>thread</p>
    </RetroWindow>,
  );

  assert.ok(markup.includes('win-draggable'), 'the desk window is a positioned, draggable frame');
  assert.ok(markup.includes('win-title'), 'the title bar must name the window');
  assert.ok(markup.includes('reason008'), 'the title bar must carry the window’s title');
  assert.ok(markup.includes('aria-label="Minimize window"'), 'the minimize control is missing');
  assert.ok(markup.includes('aria-label="Maximize window"'), 'the maximize control is missing');
  assert.ok(markup.includes('aria-label="Close window"'), 'the close control is missing');
  // A resizable window (numeric size) grows from three edges.
  for (const handle of ['rz-e', 'rz-s', 'rz-se']) {
    assert.ok(markup.includes(handle), `the ${handle} resize handle is missing`);
  }
  assert.ok(!markup.includes('win-inactive'), 'the focused window must not render as inactive');
});

test('a minimized window renders nothing; an unfocused one dims its bar', () => {
  const minimized = renderToStaticMarkup(
    <RetroWindow
      title="a"
      x={0}
      y={0}
      z={21}
      active={false}
      width={400}
      height={320}
      minimized
      onFocus={NOOP}
      onMinimize={NOOP}
      onClose={NOOP}
      onMove={NOOP}
    >
      <p>thread</p>
    </RetroWindow>,
  );
  assert.equal(minimized, '', 'a minimized window renders nothing — its tab is what remains');

  const inactive = renderToStaticMarkup(
    <RetroWindow
      title="a"
      x={0}
      y={0}
      z={21}
      active={false}
      width={400}
      height={320}
      onFocus={NOOP}
      onMinimize={NOOP}
      onClose={NOOP}
      onMove={NOOP}
    >
      <p>thread</p>
    </RetroWindow>,
  );
  assert.ok(inactive.includes('win-inactive'), 'an unfocused window must render as inactive');
});

test('a phone window has no chrome: the strip’s tab already names and closes it', () => {
  const markup = renderToStaticMarkup(
    <RetroWindow
      title="reason008"
      x={0}
      y={0}
      z={21}
      active
      width={520}
      height={460}
      mobileFullscreen
      onFocus={NOOP}
      onMinimize={NOOP}
      onClose={NOOP}
      onMove={NOOP}
    >
      <p>thread</p>
    </RetroWindow>,
  );

  assert.ok(markup.includes('mtab-window'), 'the phone window is the strip’s full-bleed surface');
  assert.ok(!markup.includes('win-titlebar'), 'a phone window has no title bar');
  assert.ok(!markup.includes('win-ctl'), 'a phone window has no window controls');
  assert.ok(!markup.includes('rz-handle'), 'a phone window cannot be resized');
});

// --- the taskbar ---

test('the taskbar offers one button per window, the focused one marked', () => {
  const markup = desk(
    <Taskbar
      windows={[
        win({ id: 'chat:c1', kind: 'chat', title: 'reason008' }),
        win({ id: 'store', kind: 'store', title: 'Store' }),
      ]}
      activeId="chat:c1"
      onlineSince={Date.now()}
      onToggle={NOOP}
      onRequestLogout={NOOP}
      accountName="Ada"
      pos="bottom"
      onTogglePos={NOOP}
    />,
  );

  const buttons = markup.match(/class="task-btn( task-btn-active)?"/g) ?? [];
  assert.equal(buttons.length, 2, 'one button per open window, nothing else');
  assert.equal(
    (markup.match(/task-btn-active/g) ?? []).length,
    1,
    'exactly one button may carry the active mark',
  );
  assert.ok(markup.includes('reason008'), 'the window’s title must be on its button');
  assert.ok(markup.includes('task-btn-kind'), 'the button names what kind of thing it opens');
  assert.ok(markup.includes('taskbar-brand'), 'the taskbar carries the brand');
});

test('a minimized window keeps its button, with the pale dot', () => {
  const markup = desk(
    <Taskbar
      windows={[win({ id: 'chat:c1', kind: 'chat', title: 'reason008', minimized: true })]}
      activeId={null}
      onlineSince={Date.now()}
      onToggle={NOOP}
      onRequestLogout={NOOP}
      accountName="Ada"
      pos="bottom"
      onTogglePos={NOOP}
    />,
  );

  assert.ok(markup.includes('task-dot-min'), 'the minimized window’s dot is the pale one');
  assert.ok(!markup.includes('task-btn-active'), 'a minimized window is not the active one');
});

test('an unanswered balance stays silent; the clock and the logout are always there', () => {
  const markup = desk(
    <Taskbar
      windows={[]}
      activeId={null}
      onlineSince={Date.now()}
      onToggle={NOOP}
      onRequestLogout={NOOP}
      accountName="Ada"
      pos="bottom"
      onTogglePos={NOOP}
    />,
  );

  // The balance is read once per mount; a wallet that has not answered says nothing — a
  // silence the static render pins, because "0" would be a balance the wallet never reported.
  assert.ok(!markup.includes('$MIG balance'), 'an unread balance must not render a chip');
  assert.ok(markup.includes('aria-label="Clock"'), 'the clock is missing');
  assert.ok(markup.includes('Logout'), 'the logout control is missing');
  assert.ok(
    markup.includes('aria-label="Move taskbar to top"'),
    'the dock toggle must name the edge it moves to',
  );
});

test('the taskbar can be docked to the top edge, and the toggle names the way back', () => {
  const markup = desk(
    <Taskbar
      windows={[]}
      activeId={null}
      onlineSince={Date.now()}
      onToggle={NOOP}
      onRequestLogout={NOOP}
      accountName="Ada"
      pos="top"
      onTogglePos={NOOP}
    />,
  );

  assert.ok(markup.includes('taskbar-top'), 'the top dock must carry its variant class');
  assert.ok(
    markup.includes('aria-label="Move taskbar to bottom"'),
    'the toggle must offer the way back down',
  );
});

// --- the phone's strip ---

/** The strip over the given home-tab state, with no windows unless a test adds them. */
function strip(fields?: {
  windows?: readonly WinState[];
  activeId?: string | null;
  unreadWin?: Readonly<Record<string, number>>;
  navTab?: MobileNavTab;
  hiddenNavs?: readonly MobileNavTab[];
  navUnread?: Readonly<Record<MobileNavTab, number>>;
}): string {
  return renderToStaticMarkup(
    <MobileTabBar
      windows={fields?.windows ?? []}
      activeId={fields?.activeId ?? null}
      unreadWin={fields?.unreadWin ?? {}}
      navTab={fields?.navTab ?? 'feed'}
      hiddenNavs={fields?.hiddenNavs ?? []}
      navUnread={fields?.navUnread ?? { friends: 0, rooms: 0, feed: 0 }}
      onSelectNav={NOOP}
      onCloseNav={NOOP}
      onReopenNav={NOOP}
      onSelectWindow={NOOP}
      onCloseWindow={NOOP}
    />,
  );
}

test('the strip offers the home tabs in the reference order, and only Feed closes', () => {
  const markup = strip();

  let at = -1;
  for (const label of MOBILE_NAV_ORDER.map(
    (tab) => ({ friends: 'Friends', rooms: 'Rooms', feed: 'Feed' })[tab],
  )) {
    const found = markup.indexOf(`>${label}</button>`);
    assert.ok(found !== -1, `the "${label}" home tab is missing from the strip`);
    assert.ok(found > at, `the "${label}" tab is out of the reference order`);
    at = found;
  }
  // Friends and Rooms are the home itself: they ship without an X. Feed is one surface among
  // three, so it is the one a person may close.
  assert.ok(markup.includes('aria-label="Close Feed tab"'), 'the Feed tab must be closable');
  assert.ok(!markup.includes('Close Friends'), 'the Friends tab must not be closable');
  assert.ok(!markup.includes('Close Rooms'), 'the Rooms tab must not be closable');
  assert.ok(!markup.includes('Reopen closed tabs'), 'no "+" while every home tab is open');
  assert.ok(markup.includes('mtab-divider'), 'the divider between home and window tabs is missing');
});

test('a closed home tab comes back through the "+"', () => {
  const markup = strip({ hiddenNavs: ['feed'] });

  assert.ok(markup.includes('aria-label="Reopen closed tabs"'), 'the "+" must offer the reopen');
  assert.ok(
    !markup.includes('aria-label="Close Feed tab"'),
    'a closed tab is gone from the strip, not merely marked',
  );
});

test('one closable tab per window, its unread badge capped at nine-plus', () => {
  const markup = strip({
    windows: [
      win({ id: 'chat:c1', kind: 'chat', title: 'reason008' }),
      win({ id: 'chat:c2', kind: 'chat', title: 'vela', minimized: true }),
    ],
    activeId: 'chat:c1',
    unreadWin: { 'chat:c1': 12, 'chat:c2': 2 },
  });

  assert.ok(markup.includes('aria-label="Close reason008"'), 'every window tab must be closable');
  assert.ok(markup.includes('mtab-title'), 'the window tab must carry the window’s title');
  assert.ok(markup.includes('>9+</span>'), 'a dozen unread reads as "many", not as arithmetic');
  assert.ok(markup.includes('>2</span>'), 'a small unread count reads as itself');
  assert.ok(markup.includes('task-dot-min'), 'a parked window’s tab shows the pale dot');
  assert.equal(
    (markup.match(/task-btn-active/g) ?? []).length,
    1,
    'exactly one tab may carry the active mark',
  );
});

// --- the vocabulary ---

test('every window kind has a label and an icon, and a chat window ids by conversation', () => {
  const kinds: readonly WinKind[] = [
    'chat',
    'notifications',
    'search',
    'wallet',
    'profile',
    'account',
    'settings',
    'admins',
    'store',
    'games',
  ];
  for (const kind of kinds) {
    assert.ok(KIND_LABEL[kind].length > 0, `the "${kind}" window has no taskbar label`);
    assert.ok(KIND_ICON[kind].length > 0, `the "${kind}" window has no tab icon`);
  }
  assert.equal(chatWinId('c1' as Id), 'chat:c1', 'a chat window’s id is its conversation’s');
  assert.equal(WINDOW_SIZES.w, 400, 'the side window’s width is the design’s own');
  assert.equal(STORE_WINDOW.w, 430, 'the store window keeps its wider cut');
});

// --- the contacts window ---

test('the contacts list is a window: titled, pill-navigated, and closing it asks to leave', () => {
  const markup = sessionShell(
    <ContactsWindow
      tab="friends"
      onTabChange={NOOP}
      width={360}
      height={560}
      maximized={false}
      onMinimize={NOOP}
      onToggleMaximize={NOOP}
      onClose={NOOP}
      onResize={NOOP}
      onOpenWindow={NOOP}
      onOpenConversation={NOOP}
    />,
  );

  assert.ok(markup.includes('contacts-frame'), 'the contacts list must be a window frame');
  assert.ok(markup.includes('>Contacts</span>'), 'the title bar must name the window');
  assert.ok(
    markup.includes('aria-label="Close and log out"'),
    'the close control must ask to log out',
  );
  let at = -1;
  for (const label of ['Friends', 'Rooms', 'Feed']) {
    const labelAt = markup.indexOf(`>${label}</button>`);
    assert.ok(labelAt !== -1, `the "${label}" pill is missing from the nav`);
    assert.ok(labelAt > at, `the "${label}" pill is out of the reference order`);
    at = labelAt;
  }
  assert.ok(markup.includes('hdr-orange'), 'the me bar is missing from the window');
  assert.ok(markup.includes('New here! Say hi :)'), 'the status line owes its placeholder');
  assert.ok(markup.includes('title="Menu"'), 'the gear menu button is missing');
});
