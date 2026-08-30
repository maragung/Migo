/**
 * What the top navigation offers as the app's sections.
 *
 * The bar is the app's whole navigation, so its offer is its contract: seven sections, Settings
 * last, each button carrying the label and the current-page attribute a screen reader needs. A
 * tab that vanished would strand its panel behind no control at all. The bar also carries the
 * two controls that are not sections — the account chip that opens the profile and the theme
 * toggle — so their presence is pinned here too.
 *
 * The component reads the signed-in account from the Migo context, so the renderer is fed a
 * minimal context double the way `calls.test.tsx` feeds its manager; `renderToStaticMarkup` runs
 * no effects, so the profile lookup never fires and the chip falls back to its "You" label.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { TopNav } from '../src/components/top-nav.js';
import type { AppTab } from '../src/components/top-nav.js';
import { MigoContext } from '../src/lib/migo/provider.js';

/** Renders the bar under a ready-session context double with a known account. */
function render(nav: ReactNode): string {
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
      {nav}
    </MigoContext.Provider>,
  );
}

test('the bar offers seven sections, Settings among them', () => {
  const markup = render(<TopNav active="chats" onSelect={() => {}} />);

  const buttons = markup.match(/<button[^>]*class="top-nav-tab[^"]*"/g) ?? [];
  assert.equal(buttons.length, 7, 'the bar must offer exactly seven sections');

  for (const label of ['Chats', 'Friends', 'Alerts', 'Discover', 'Gifts', 'Profile', 'Settings']) {
    assert.ok(
      markup.includes(`top-nav-tab-label">${label}</span>`),
      `the "${label}" section is missing`,
    );
  }
  assert.ok(markup.includes('⚙️'), 'the settings section lost its glyph');
});

test('the active section carries the current-page attribute and the active style', () => {
  const markup = render(<TopNav active="settings" onSelect={() => {}} />);

  // The active mark appears once per bar (top bar and mobile bottom bar) and nowhere else.
  assert.equal(
    (markup.match(/class="top-nav-tab active"/g) ?? []).length,
    1,
    'exactly one top-bar section may be active',
  );
  assert.equal(
    (markup.match(/class="bottom-nav-btn active"/g) ?? []).length,
    1,
    'exactly one bottom-bar section may be active',
  );
  const activeButton = markup.match(/<button[^>]*aria-current="page"[^>]*>[\s\S]*?Settings/);
  assert.ok(activeButton !== null, 'no section is marked as the current page');
  assert.ok(
    activeButton[0].includes('class="top-nav-tab active"') ||
      activeButton[0].includes('class="bottom-nav-btn active"'),
    'the current section lost its active style',
  );
  assert.ok(
    markup.includes('top-nav-tab-label">Settings</span>'),
    'the current section is Settings',
  );
  // Two bars, one shared truth: each marks exactly the one current section.
  assert.equal((markup.match(/aria-current="page"/g) ?? []).length, 2);
});

test('every section type the layout switches on has a bar button', () => {
  const tabs: AppTab[] = [
    'chats',
    'friends',
    'notifications',
    'discover',
    'gifts',
    'profile',
    'settings',
  ];
  for (const tab of tabs) {
    const markup = render(<TopNav active={tab} onSelect={() => {}} />);
    assert.ok(
      (markup.match(/class="top-nav-tab active"/g) ?? []).length === 1,
      `the "${tab}" section could be marked active, so it must exist on the bar`,
    );
  }
});

test('the bar carries the account chip and the theme control', () => {
  const markup = render(<TopNav active="chats" onSelect={() => {}} />);

  // The chip is a labelled control that opens the profile, not decoration.
  assert.ok(
    markup.includes('aria-label="Open your profile"'),
    'the account chip lost its profile-opening label',
  );
  // The theme control names the theme a click would move to; the render seeds dark, so the
  // offer is the light one.
  assert.ok(
    markup.includes('aria-label="Switch to light theme"'),
    'the theme control is missing or mislabelled',
  );
});

test('the theme control follows the theme it is handed', () => {
  const dark = render(<TopNav active="chats" onSelect={() => {}} theme="dark" />);
  assert.ok(dark.includes('aria-label="Switch to light theme"'));

  const light = render(<TopNav active="chats" onSelect={() => {}} theme="light" />);
  assert.ok(light.includes('aria-label="Switch to dark theme"'));
});
