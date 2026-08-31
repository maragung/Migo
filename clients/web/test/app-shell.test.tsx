/**
 * What the messenger shell offers as the app's navigation.
 *
 * The v0.9.0 shell is the reference's tab strip plus profile banner: five system tabs —
 * Friends, Chats, Rooms, Games, Feed, in that order — one closable chip per open conversation,
 * one closable chip per open secondary panel, and the banner that owns the account (the avatar
 * menu, the theme control). The shell is the app's whole navigation, so its offer is its
 * contract: a surface that vanished would strand its panel behind no control at all.
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
import type { ChatTabChip, PanelTab } from '../src/components/app-shell.js';
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
  onSelectSystem: () => {},
  onSelectChat: (_: Id) => {},
  onSelectPanel: (_: PanelTab) => {},
  onCloseChat: (_: Id) => {},
  onClosePanel: (_: PanelTab) => {},
  onOpenPanel: (_: PanelTab) => {},
};

test('the strip offers the five system tabs in the reference order', () => {
  const markup = render(
    <AppShell active="chats" chatTabs={[]} panelTabs={[]} {...NOOP}>
      <p>content</p>
    </AppShell>,
  );

  // The pattern anchors on the whole class attribute so the icon/label spans inside a chip
  // (tab-chip-icon, tab-chip-label) never count — only the chip buttons do.
  const chips = markup.match(/class="tab-chip( active| tab-chat| tab-panel)*"/g) ?? [];
  assert.equal(chips.length, 5, 'five system tabs, nothing else when nothing is open');
  let at = -1;
  for (const label of ['Friends', 'Chats', 'Rooms', 'Games', 'Feed']) {
    const found = markup.indexOf(`tab-chip-label">${label}</span>`);
    assert.ok(found !== -1, `the "${label}" tab is missing from the strip`);
    assert.ok(found > at, `the "${label}" tab is out of the reference order`);
    at = found;
  }
});

test('an open conversation is a closable chip on the strip', () => {
  const chatTabs: ChatTabChip[] = [{ conversationId: 'c1' as Id, title: 'reason008' }];
  const markup = render(
    <AppShell active="chat:c1" chatTabs={chatTabs} panelTabs={[]} {...NOOP}>
      <p>content</p>
    </AppShell>,
  );

  assert.ok(
    markup.includes('tab-chip-label">reason008</span>'),
    'the conversation chip must carry its title',
  );
  assert.ok(
    markup.includes('aria-label="Close reason008"'),
    'the conversation chip must be closable',
  );
});

test('an open panel is a closable chip on the strip', () => {
  const markup = render(
    <AppShell active="panel:wallet" chatTabs={[]} panelTabs={['wallet']} {...NOOP}>
      <p>content</p>
    </AppShell>,
  );

  assert.ok(markup.includes('tab-chip-label">Wallet</span>'), 'the panel chip is missing');
  assert.ok(markup.includes('aria-label="Close Wallet"'), 'the panel chip must be closable');
});

test('the active tab carries the current-page attribute exactly once', () => {
  const chatTabs: ChatTabChip[] = [
    { conversationId: 'c1' as Id, title: 'a' },
    { conversationId: 'c2' as Id, title: 'b' },
  ];
  const markup = render(
    <AppShell active="chat:c2" chatTabs={chatTabs} panelTabs={['settings']} {...NOOP}>
      <p>content</p>
    </AppShell>,
  );

  const chips = markup.match(/class="tab-chip( active| tab-chat| tab-panel)*"/g) ?? [];
  const active = markup.match(/class="tab-chip( tab-[a-z]+)? active"/g) ?? [];
  assert.equal(chips.length, 8, 'five system tabs plus the two chat chips and the panel chip');
  assert.equal(active.length, 1, 'exactly one chip may be active');
  assert.equal(
    (markup.match(/aria-current="page"/g) ?? []).length,
    1,
    'the active tab is the one truth',
  );
});

test('the banner carries the account menu and the theme control', () => {
  const markup = render(
    <AppShell active="chats" chatTabs={[]} panelTabs={[]} {...NOOP}>
      <p>content</p>
    </AppShell>,
  );

  const menuButtons = markup.match(/aria-label="Open the account menu"/g) ?? [];
  assert.equal(menuButtons.length, 1, 'the banner must own exactly one account menu control');
  assert.ok(
    markup.includes('aria-label="Switch to light theme"'),
    'the theme control is missing or mislabelled',
  );
});

test('the theme control follows the theme it is handed', () => {
  const dark = render(
    <AppShell active="chats" chatTabs={[]} panelTabs={[]} theme="dark" {...NOOP}>
      <p>content</p>
    </AppShell>,
  );
  const light = render(
    <AppShell active="chats" chatTabs={[]} panelTabs={[]} theme="light" {...NOOP}>
      <p>content</p>
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
