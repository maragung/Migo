/**
 * What the messenger shell offers as the app's sections.
 *
 * The shell is the app's whole navigation, so its offer is its contract — and the contract is
 * the spec's: the FRIENDS | CHATS | ROOMS trio first (the three lists a messenger lives in),
 * then the stream and the destinations, Profile and Settings in the foot. The mobile bar keeps
 * to five slots (the trio, Space, More), and More opens a dialog rather than switching a
 * section. A section that vanished would strand its panel behind no control at all.
 *
 * The shell reads the signed-in account from the Migo context, so the renderer is fed a minimal
 * context double the way `calls.test.tsx` feeds its manager; `renderToStaticMarkup` runs no
 * effects, so the profile lookup never fires and the chip falls back to its "You" label.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { AppShell } from '../src/components/app-shell.js';
import type { AppTab } from '../src/components/app-shell.js';
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

/** Every section the layout can switch on — one rail button per section must exist somewhere. */
const ALL_TABS: AppTab[] = [
  'friends',
  'chats',
  'rooms',
  'space',
  'notifications',
  'search',
  'wallet',
  'profile',
  'settings',
];

test('the rail offers every section; the trio sits in its own group above the divider', () => {
  const markup = render(
    <AppShell active="chats" onSelect={() => {}}>
      <p>content</p>
    </AppShell>,
  );

  // Nine sections, one rail button each. The pattern anchors on the button element so the
  // icon/label spans inside never count.
  const railButtons = markup.match(/<button[^>]*class="rail-btn[^"]*"/g) ?? [];
  assert.equal(railButtons.length, 10, 'nine sections plus the brand button');
  // The trio is its own tablist — the primary list group, not one option among many.
  assert.ok(markup.includes('role="tablist"'), 'the FRIENDS|CHATS|ROOMS trio must be a tab group');
  const trio = markup.match(/role="tablist"[\s\S]*?role="tablist"/) ?? [];
  assert.equal(trio.length, 0, 'exactly one trio group');
});

test('the mobile bar keeps to five slots: the trio, Space, More', () => {
  const markup = render(
    <AppShell active="chats" onSelect={() => {}}>
      <p>content</p>
    </AppShell>,
  );

  const buttons = markup.match(/class="bottom-nav-btn[^"]*"/g) ?? [];
  assert.equal(buttons.length, 5, 'the bottom bar must keep to five slots');
  for (const label of ['Friends', 'Chats', 'Rooms', 'Space', 'More']) {
    assert.ok(
      markup.includes(`bottom-nav-label">${label}</span>`),
      `the "${label}" slot is missing from the bottom bar`,
    );
  }
  // More opens a sheet, not a section: it announces the dialog it has.
  assert.ok(
    markup.includes('aria-haspopup="dialog"'),
    'the More slot must open a dialog, not switch sections',
  );
});

test('the active section carries the current-page attribute exactly once per surface', () => {
  const markup = render(
    <AppShell active="rooms" onSelect={() => {}}>
      <p>content</p>
    </AppShell>,
  );

  assert.equal(
    (markup.match(/class="rail-btn active"/g) ?? []).length,
    1,
    'exactly one rail section may be active',
  );
  assert.equal(
    (markup.match(/class="bottom-nav-btn active"/g) ?? []).length,
    1,
    'exactly one bottom-bar section may be active',
  );
  assert.equal((markup.match(/aria-current="page"/g) ?? []).length, 2, 'two surfaces, one truth');
});

test('every section type the layout switches on has a rail button', () => {
  for (const tab of ALL_TABS) {
    const markup = render(
      <AppShell active={tab} onSelect={() => {}}>
        <p>content</p>
      </AppShell>,
    );
    assert.equal(
      (markup.match(/class="rail-btn active"/g) ?? []).length,
      1,
      `the "${tab}" section could be marked active, so it must exist on the rail`,
    );
  }
});

test('the shell carries the account control and the theme control', () => {
  const markup = render(
    <AppShell active="chats" onSelect={() => {}}>
      <p>content</p>
    </AppShell>,
  );

  const profileButtons = markup.match(/aria-label="Open your profile"/g) ?? [];
  assert.ok(profileButtons.length >= 1, 'the account control lost its profile-opening label');
  assert.ok(
    markup.includes('aria-label="Switch to light theme"'),
    'the theme control is missing or mislabelled',
  );
});

test('the theme control follows the theme it is handed', () => {
  const dark = render(
    <AppShell active="chats" onSelect={() => {}} theme="dark">
      <p>content</p>
    </AppShell>,
  );
  const light = render(
    <AppShell active="chats" onSelect={() => {}} theme="light">
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

test('an open thread folds the mobile header away', () => {
  const markup = render(
    <AppShell active="chats" onSelect={() => {}} hasThread>
      <p>content</p>
    </AppShell>,
  );
  assert.ok(
    markup.includes('app-thread-open'),
    'the shell must tell the stylesheet a thread is open',
  );
});
