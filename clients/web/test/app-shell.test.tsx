/**
 * What the messenger shell offers as the app's navigation.
 *
 * The right pane has one mode now, not two: a single tab bar whose first chip is the Feed —
 * the pane's resting content, always present, never closable — followed by one closable chip
 * per open thing: a conversation, the games arcade, or a secondary panel the banner menu or a
 * deep link reached. There is no "menu panel" to switch back to: closing a chip falls through
 * to the next one, and closing the last one leaves the Feed, which is exactly the fallback an
 * empty pane owes. The shell is the app's whole navigation, so its offer is its contract.
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
import type { PanelTab, RightTabChip, SystemTab } from '../src/components/app-shell.js';
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
  onSelectRight: (_: string) => {},
  onCloseRight: (_: string) => {},
  onBackToLists: () => {},
  onOpenPanel: (_: PanelTab) => {},
};

/**
 * The shell with the given right-pane tabs. `active` is the pane's active chip (`'feed'` or a
 * tab id); `activeChat` names the conversation whose thread the pane is showing, when the
 * active tab is a chat.
 */
function shell(
  tabs: readonly RightTabChip[],
  active: string,
  activeChat: Id | null = null,
): ReactNode {
  return (
    <AppShell
      leftTab="friends"
      leftContent={<p>left</p>}
      rightTabs={tabs}
      activeRight={active}
      activeChat={activeChat}
      rightContent={<p>right</p>}
      showRight={activeChat !== null}
      {...NOOP}
    >
      <p>thread</p>
    </AppShell>
  );
}

test('the left strip offers the four system tabs in the reference order', () => {
  const markup = render(shell([], 'feed'));

  // The pattern anchors on the whole class attribute so the icon/label spans inside a chip
  // (tab-chip-icon, tab-chip-label) never count — only the chip buttons do.
  const chips = markup.match(/class="tab-chip( active)?"/g) ?? [];
  assert.equal(chips.length, 4, 'four system tabs on the left strip, nothing else');
  let at = -1;
  for (const label of ['Main', 'Rooms', 'Games', 'Feed']) {
    const found = markup.indexOf(`tab-chip-label">${label}</span>`);
    assert.ok(found !== -1, `the "${label}" tab is missing from the strip`);
    assert.ok(found > at, `the "${label}" tab is out of the reference order`);
    at = found;
  }
});

test('an empty right pane still offers the Feed, and never a menu panel', () => {
  const markup = render(shell([], 'feed'));

  // The resting chip is the bar's own — an empty pane still owes the Feed.
  assert.ok(
    markup.includes('class="chat-tab active"'),
    'the Feed chip must be present and active when nothing is open',
  );
  assert.ok(markup.includes('aria-label="Open panels"'), 'the right pane must carry its tab bar');
  assert.ok(!markup.includes('Menu Panel'), 'the menu panel is gone: the pane is tabs only');
  assert.ok(!markup.includes('Panel: '), 'the pane no longer names a two-mode "panel" it shows');
});

test('every open thing is a closable chip beside the Feed', () => {
  const tabs: RightTabChip[] = [
    { id: 'chat:c1', kind: 'chat', conversationId: 'c1' as Id, title: 'reason008' },
    { id: 'games', kind: 'games', title: 'Games' },
    { id: 'wallet', kind: 'wallet', title: 'TopUp' },
  ];
  const markup = render(shell(tabs, 'chat:c1', 'c1' as Id));

  assert.ok(
    markup.includes('tab-chip-label">reason008</span>'),
    'the conversation chip must carry its title',
  );
  assert.ok(
    markup.includes('aria-label="Close reason008"'),
    'the conversation chip must be closable',
  );
  assert.ok(markup.includes('aria-label="Close TopUp"'), 'the panel chips must be closable too');
  // The resting chip is never closable — closing everything must leave the Feed.
  assert.ok(
    !markup.includes('Close Feed'),
    "the Feed chip has no close control; it is the pane's fallback",
  );
  // The chat tab is the active one, so the pane shows the thread, not the right content.
  assert.ok(markup.includes('class="thread-area"'), 'the active chat tab shows the thread');
  assert.ok(!markup.includes('<p>right</p>'), 'the pane does not also render its fallback');
});

test('exactly one chip is active per pane, never more', () => {
  const tabs: RightTabChip[] = [
    { id: 'chat:c1', kind: 'chat', conversationId: 'c1' as Id, title: 'a' },
    { id: 'chat:c2', kind: 'chat', conversationId: 'c2' as Id, title: 'b' },
  ];
  const markup = render(shell(tabs, 'chat:c2', 'c2' as Id));

  // Two open conversations plus the bar's own Feed chip.
  const chatChips = markup.match(/class="chat-tab( active)?"/g) ?? [];
  assert.equal(chatChips.length, 3, 'one chip per open thing, plus the Feed');
  assert.equal(markup.match(/class="chat-tab active"/g)?.length ?? 0, 1, 'one active chip');
  assert.equal(
    (markup.match(/aria-current="page"/g) ?? []).length,
    2,
    'exactly one current page per pane: the left strip tab and the active right chip',
  );
});

test('the back control is icon-only and never says menu', () => {
  const markup = render(shell([], 'feed'));

  assert.ok(
    markup.includes('aria-label="Back to the lists"'),
    'the single-column way home to the lists is on the bar',
  );
  assert.ok(!markup.includes('class="chat-back pane-back"'), 'the old two-mode back is gone');
});

test('the banner carries the account menu and the theme control', () => {
  const markup = render(shell([], 'feed'));

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
      leftTab="friends"
      leftContent={<p>left</p>}
      rightTabs={[]}
      activeRight="feed"
      activeChat={null}
      rightContent={<p>right</p>}
      showRight={false}
      theme="dark"
      {...NOOP}
    >
      <p>thread</p>
    </AppShell>,
  );
  const light = render(
    <AppShell
      leftTab="friends"
      leftContent={<p>left</p>}
      rightTabs={[]}
      activeRight="feed"
      activeChat={null}
      rightContent={<p>right</p>}
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
