/**
 * What the messenger shell offers as the app's navigation.
 *
 * The new-ui-02 shell is two independent panels: a left panel with its own tab strip (Friends,
 * Chats, Rooms, Games, Feed — the lists and streams a messenger lives in) over the orange
 * profile banner, and a right panel that runs on its own state — its menu tab bar (Feed, Games,
 * Alerts, Search, TopUp, Profile, Settings) when no conversation is active, or the chat tab bar
 * with one closable chip per open conversation and the "‹ Menu Panel" way back when one is.
 * The shell is the app's whole navigation, so its offer is its contract: a surface that
 * vanished would strand its panel behind no control at all.
 *
 * The shell reads the signed-in account from the Migo context, so the renderer is fed a minimal
 * context double the way `calls.test.tsx` feeds its manager; `renderToStaticMarkup` runs no
 * effects, so the profile lookup never fires and the name surfaces fall back to their "You"
 * labels.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { AppShell } from '../src/components/app-shell.js';
import type { ChatTabChip, PanelTab, RightTab, SystemTab } from '../src/components/app-shell.js';
import { MigoContext } from '../src/lib/migo/provider.js';

/** The shell under a ready-session context double with a known account. */
function render(shell: ReactNode): string {
  return renderToStaticMarkup(
    <MigoContext.Provider
      value={{
        status: 'ready',
        connectionState: 'ready',
        accountId: 'acct_self' as Id,
        deviceId: null,
        error: null,
        resetNonce: 0,
        persistKeyStore: () => {},
        client: null,
        register: () => Promise.resolve(),
        login: () => Promise.resolve(),
        logout: () => Promise.resolve(),
      }}
    >
      {shell}
    </MigoContext.Provider>,
  );
}

/** The shell's no-op callbacks: the tests assert on structure, never on navigation. */
const NOOP = {
  onSelectSystem: (_: SystemTab) => {},
  onSelectRightTab: (_: RightTab) => {},
  onSelectChat: (_: Id) => {},
  onCloseChat: (_: Id) => {},
  onBackToMenu: () => {},
  onOpenPanel: (_: PanelTab) => {},
};

/** The shell with one conversation open, the right pane's chat mode. */
function chatShell(tabs: ChatTabChip[], active: Id | null): ReactNode {
  return (
    <AppShell
      leftTab="chats"
      leftContent={<p>left</p>}
      rightTab="feed"
      rightContent={<p>right</p>}
      activeChat={active}
      chatTabs={tabs}
      showRight={active !== null}
      {...NOOP}
    >
      <p>thread</p>
    </AppShell>
  );
}

test('the left strip offers the five system tabs in the reference order', () => {
  const markup = render(chatShell([], null));

  // The pattern anchors on the whole class attribute so the icon/label spans inside a chip
  // (tab-chip-icon, tab-chip-label) never count — only the chip buttons do.
  const chips = markup.match(/class="tab-chip( active)?"/g) ?? [];
  assert.equal(chips.length, 5, 'five system tabs on the left strip, nothing else');
  let at = -1;
  for (const label of ['Friends', 'Chats', 'Rooms', 'Games', 'Feed']) {
    const found = markup.indexOf(`tab-chip-label">${label}</span>`);
    assert.ok(found !== -1, `the "${label}" tab is missing from the strip`);
    assert.ok(found > at, `the "${label}" tab is out of the reference order`);
    at = found;
  }
});

test('the right pane in menu mode offers its own panel tabs', () => {
  const markup = render(chatShell([], null));

  assert.ok(markup.includes('Panel: Feed'), 'the menu bar must name the pane it is showing');
  for (const label of ['Feed', 'Games', 'Alerts', 'Search', 'TopUp', 'Profile', 'Settings']) {
    assert.ok(
      markup.includes(`>${label}</button>`),
      `the "${label}" panel tab is missing from the menu bar`,
    );
  }
  // The single-column story: the menu pane carries its own way back to the left panel.
  assert.ok(markup.includes('class="chat-back pane-back"'), 'the menu pane back control');
});

test('an open conversation is a closable chip on the chat bar', () => {
  const chatTabs: ChatTabChip[] = [{ conversationId: 'c1' as Id, title: 'reason008' }];
  const markup = render(chatShell(chatTabs, 'c1' as Id));

  assert.ok(
    markup.includes('class="chat-tab active"'),
    'the active conversation chip must mark itself',
  );
  assert.ok(
    markup.includes('tab-chip-label">reason008</span>'),
    'the conversation chip must carry its title',
  );
  assert.ok(
    markup.includes('aria-label="Close reason008"'),
    'the conversation chip must be closable',
  );
  // The chat mode's way back: the mockup's cyan "‹ Menu Panel" control, always on the bar.
  assert.ok(markup.includes('Menu Panel</span>'), 'the chat bar must offer the menu panel');
});

test('exactly one chip is active per pane, never more', () => {
  const chatTabs: ChatTabChip[] = [
    { conversationId: 'c1' as Id, title: 'a' },
    { conversationId: 'c2' as Id, title: 'b' },
  ];
  const markup = render(chatShell(chatTabs, 'c2' as Id));

  const chatChips = markup.match(/class="chat-tab( active)?"/g) ?? [];
  assert.equal(chatChips.length, 2, 'one chip per open conversation');
  assert.equal(markup.match(/class="chat-tab active"/g)?.length ?? 0, 1, 'one active chat chip');
  assert.equal(
    (markup.match(/aria-current="page"/g) ?? []).length,
    2,
    'exactly one current page per pane: the left strip tab and the active chat chip',
  );
});

test('the banner carries the account menu and the theme control', () => {
  const markup = render(chatShell([], null));

  const menuButtons = markup.match(/aria-label="Open the account menu"/g) ?? [];
  assert.equal(menuButtons.length, 1, 'the banner must own exactly one account menu control');
  assert.ok(
    markup.includes('aria-label="Switch to light theme"'),
    'the theme control is missing or mislabelled',
  );
});

test('the theme control follows the theme it is handed', () => {
  const dark = render(
    <AppShell
      leftTab="chats"
      leftContent={<p>left</p>}
      rightTab="feed"
      rightContent={<p>right</p>}
      activeChat={null}
      chatTabs={[]}
      showRight={false}
      theme="dark"
      {...NOOP}
    >
      <p>thread</p>
    </AppShell>,
  );
  const light = render(
    <AppShell
      leftTab="chats"
      leftContent={<p>left</p>}
      rightTab="feed"
      rightContent={<p>right</p>}
      activeChat={null}
      chatTabs={[]}
      showRight={false}
      theme="light"
      {...NOOP}
    >
      <p>thread</p>
    </AppShell>,
  );

  assert.ok(
    dark.includes('aria-label="Switch to light theme"'),
    'the dark theme must offer the light one',
  );
  assert.ok(
    light.includes('aria-label="Switch to dark theme"'),
    'the light theme must offer the dark one',
  );
});
