/**
 * What Chat List Mode is allowed to be.
 *
 * The mode is additive by contract: the tabbed layout it sits beside is the stable baseline, and
 * the mode's own surfaces — the settings control that picks it, the full-screen chat activity a
 * tap opens, the searchable conversation list — must each state exactly what they are and
 * nothing more. These tests pin that at the component layer:
 *
 *   1. **The settings control.** Settings → Navigation offers exactly the two named choices, and
 *      the pressed one is the stored one — tabbed when nothing is stored (the default an account
 *      that never opened Settings keeps seeing), Chat List Mode when the store says so.
 *   2. **The chat activity.** The thread a tap opens is a full-screen activity of its own, with
 *      the back control that returns to the list — not a pane sharing a window with it, and not
 *      a small dialog over it.
 *   3. **The searchable list degrades honestly.** With no conversations and no client, the
 *      searchable wrap renders the same "no conversations yet" state the bare list does — a
 *      search field over nothing is noise, not a feature.
 */

import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { ChatActivity } from '../src/components/chat-activity.js';
import { ConversationList } from '../src/components/conversation-list.js';
import { NavigationSection } from '../src/components/settings-panel.js';
import { CallManagerProvider } from '../src/lib/migo/call-manager.js';
import { ConversationsProvider } from '../src/lib/migo/conversations-provider.js';
import { GroupCallManagerProvider } from '../src/lib/migo/group-call-manager.js';
import { MutedProvider } from '../src/lib/migo/muted-provider.js';
import { MigoContext } from '../src/lib/migo/provider.js';
import type { MigoContextValue } from '../src/lib/migo/provider.js';
import { RoomsProvider } from '../src/lib/migo/rooms-provider.js';
import { SectionNavProvider } from '../src/lib/migo/section-nav.js';

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
            <CallManagerProvider>
              <GroupCallManagerProvider>
                <SectionNavProvider navigate={() => {}}>{node}</SectionNavProvider>
              </GroupCallManagerProvider>
            </CallManagerProvider>
          </MutedProvider>
        </RoomsProvider>
      </ConversationsProvider>
    </MigoContext.Provider>,
  );
}

/** The window double: a map-backed localStorage, so the stored choice can be staged per test. */
let store: Map<string, string>;
let restoreWindow: () => void;

beforeEach(() => {
  store = new Map<string, string>();
  const target = new EventTarget();
  const win = {
    localStorage: {
      getItem: (key: string): string | null => (store.has(key) ? store.get(key)! : null),
      setItem: (key: string, value: string): void => {
        store.set(key, value);
      },
    },
    addEventListener: target.addEventListener.bind(target),
    removeEventListener: target.removeEventListener.bind(target),
    dispatchEvent: target.dispatchEvent.bind(target),
  };
  const previous = Object.getOwnPropertyDescriptor(globalThis, 'window');
  Object.defineProperty(globalThis, 'window', { configurable: true, value: win });
  restoreWindow = (): void => {
    if (previous) {
      Object.defineProperty(globalThis, 'window', previous);
    } else {
      Reflect.deleteProperty(globalThis, 'window');
    }
  };
});

afterEach(() => {
  restoreWindow();
});

// --- the settings control ---

test('Settings → Navigation offers exactly the two named choices', () => {
  const markup = renderToStaticMarkup(<NavigationSection />);

  assert.ok(markup.includes('aria-label="Navigation Mode"'), 'the group must be named');
  assert.ok(markup.includes('>Tabbed Navigation</button>'), 'the tabbed choice is missing');
  assert.ok(markup.includes('>Chat List Mode</button>'), 'the chat-list choice is missing');
  assert.equal(
    (markup.match(/aria-pressed/g) ?? []).length,
    2,
    'exactly two choices may carry a pressed state',
  );
});

test('with nothing stored, Tabbed Navigation is the pressed choice', () => {
  const markup = renderToStaticMarkup(<NavigationSection />);

  assert.ok(
    markup.includes('aria-pressed="true">Tabbed Navigation</button>'),
    'the default must be the layout the client has always had',
  );
  assert.ok(
    markup.includes('aria-pressed="false">Chat List Mode</button>'),
    'the chat-list choice must be unpressed by default',
  );
});

test('with the choice stored, the pressed one is the stored one', () => {
  store.set('migo:navMode', 'chatlist');
  const markup = renderToStaticMarkup(<NavigationSection />);

  assert.ok(
    markup.includes('aria-pressed="true">Chat List Mode</button>'),
    'a stored chat-list choice must show as pressed',
  );
  assert.ok(
    markup.includes('aria-pressed="false">Tabbed Navigation</button>'),
    'the tabbed choice must be unpressed when the store says chatlist',
  );
});

// --- the chat activity ---

test('the chat activity is a full-screen thread whose back returns to the list', () => {
  const markup = sessionShell(
    <ChatActivity conversationId={'c1' as Id} onBack={() => {}} deskTaskbar={null} />,
  );

  assert.ok(markup.includes('chat-activity'), 'the thread must wear the activity frame');
  assert.ok(
    markup.includes('aria-label="Back to chats"'),
    'the activity’s one way out is the back control',
  );
  assert.ok(
    markup.includes('thread-pane'),
    'the activity holds the same thread every surface does',
  );
});

test('the activity leaves the desk’s taskbar its own edge', () => {
  // The desk variants clear the taskbar's 34px on the edge it occupies; a phone activity (null)
  // has no variant class at all, because it covers everything.
  const bottom = sessionShell(
    <ChatActivity conversationId={'c1' as Id} onBack={() => {}} deskTaskbar="bottom" />,
  );
  assert.ok(
    bottom.includes('chat-activity-tb-bottom'),
    'a bottom-docked taskbar must keep its edge',
  );

  const top = sessionShell(
    <ChatActivity conversationId={'c1' as Id} onBack={() => {}} deskTaskbar="top" />,
  );
  assert.ok(top.includes('chat-activity-tb-top'), 'a top-docked taskbar must keep its edge');
  assert.ok(!top.includes('chat-activity-tb-bottom'), 'the variants are mutually exclusive');
});

// --- the searchable list ---

test('the searchable list renders the same honest empty state as the bare list', () => {
  const searchable = sessionShell(<ConversationList searchable />);
  const bare = sessionShell(<ConversationList />);

  assert.ok(
    searchable.includes('No conversations yet'),
    'an account with no conversations must hear that, not see a search field over nothing',
  );
  assert.equal(searchable, bare, 'with nothing to search, the wraps must agree exactly');
});
