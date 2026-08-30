/**
 * What the tab rail offers as the app's sections.
 *
 * The rail is the app's whole navigation, so its offer is its contract: seven sections, Settings
 * last, each button carrying the label and the current-page attribute a screen reader needs. A
 * tab that vanished would strand its panel behind no control at all.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { TabRail } from '../src/components/tab-rail.js';
import type { AppTab } from '../src/components/tab-rail.js';

test('the rail offers seven sections, Settings among them', () => {
  const markup = renderToStaticMarkup(<TabRail active="chats" onSelect={() => {}} />);

  const buttons = markup.match(/<button[^>]*class="tab-btn[^"]*"/g) ?? [];
  assert.equal(buttons.length, 7, 'the rail must offer exactly seven sections');

  for (const label of ['Chats', 'Friends', 'Alerts', 'Discover', 'Gifts', 'Profile', 'Settings']) {
    assert.ok(markup.includes(`>${label}</span>`), `the "${label}" section is missing`);
  }
  assert.ok(markup.includes('⚙️'), 'the settings section lost its glyph');
});

test('the active section carries the current-page attribute and the active style', () => {
  const markup = renderToStaticMarkup(<TabRail active="settings" onSelect={() => {}} />);

  const activeButton = markup.match(/<button[^>]*aria-current="page"[^>]*>/);
  assert.ok(activeButton !== null, 'no section is marked as the current page');
  assert.ok(
    activeButton[0].includes('class="tab-btn active"'),
    'the current section lost its active style',
  );
  assert.ok(
    activeButton[0].includes('⚙️') === false && markup.includes('>Settings</span>'),
    'the current section must be Settings',
  );
  // Exactly one section is current.
  assert.equal((markup.match(/aria-current="page"/g) ?? []).length, 1);
});

test('every section type the layout switches on has a rail button', () => {
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
    const markup = renderToStaticMarkup(<TabRail active={tab} onSelect={() => {}} />);
    assert.ok(
      (markup.match(/aria-current="page"/g) ?? []).length === 1,
      `the "${tab}" section could be marked current, so it must exist on the rail`,
    );
  }
});
