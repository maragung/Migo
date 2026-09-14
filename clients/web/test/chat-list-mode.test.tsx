/**
 * What Chat List Mode is allowed to be.
 *
 * The mode is additive by contract: the tabbed layout it sits beside is the stable baseline, and
 * the mode's own surfaces — the settings control that picks it, the desk's split view, the
 * searchable conversation list — must each state exactly what they are and nothing more. These
 * tests pin that at the component layer:
 *
 *   1. **The settings control.** Settings → Navigation offers exactly the two named choices, and
 *      the pressed one is the stored one — tabbed when nothing is stored (the default an account
 *      that never opened Settings keeps seeing), Chat List Mode when the store says so.
 *   2. **The desk's split view.** A left pane labelled as the chat list, a right pane labelled as
 *      the chat window, and with no conversation open the pane says so rather than rendering an
 *      empty thread that reads as one.
 *   3. **The searchable list degrades honestly.** With no conversations and no client, the
 *      searchable wrap renders the same "no conversations yet" state the bare list does — a
 *      search field over nothing is noise, not a feature.
 */

import assert from 'node:assert/strict';
import { afterEach, beforeEach, test } from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import type { ReactNode } from 'react';

import type { Id } from '@migo/sdk';

import { ChatSplitView } from '../src/components/chat-split-view.js';
import { ConversationList } from '../src/components/conversation-list.js';
import { NavigationSection } from '../src/components/settings-panel.js';
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

// --- the desk's split view ---

test('the split view is a chat list beside a chat window, and says so when nothing is open', () => {
  const markup = sessionShell(<ChatSplitView conversationId={null} />);

  assert.ok(markup.includes('aria-label="Chat list"'), 'the left pane must be the chat list');
  assert.ok(markup.includes('aria-label="Chat window"'), 'the right pane must be the chat window');
  assert.ok(
    markup.includes('No conversation open'),
    'an empty pane must say so rather than render an empty thread',
  );
  assert.ok(
    markup.includes('the list stays beside it'),
    'the empty state must name the mode’s promise: the list stays visible',
  );
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
