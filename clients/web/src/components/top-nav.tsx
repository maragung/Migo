'use client';

import type { ReactNode } from 'react';

import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfile } from '@/lib/migo/use-profiles.js';
import type { Theme } from '@/lib/theme.js';

import { Avatar } from './avatar.js';
import { ThemeToggle } from './theme-toggle.js';

/**
 * The app's top-level sections, switched by the navigation bar.
 *
 * The chat shell stays one section rather than becoming a route: the bundle is a static export and
 * the open conversation already lives in the URL fragment, so section switching is pure client
 * state and a tab change never unloads the session or the socket.
 */
export type AppTab =
  'chats' | 'friends' | 'notifications' | 'discover' | 'gifts' | 'profile' | 'settings';

const TABS: ReadonlyArray<{ id: AppTab; label: string; icon: string }> = [
  { id: 'chats', label: 'Chats', icon: '💬' },
  { id: 'friends', label: 'Friends', icon: '👥' },
  { id: 'notifications', label: 'Alerts', icon: '🔔' },
  { id: 'discover', label: 'Discover', icon: '🧭' },
  { id: 'gifts', label: 'Gifts', icon: '🎁' },
  { id: 'profile', label: 'Profile', icon: '👤' },
  { id: 'settings', label: 'Settings', icon: '⚙️' },
];

/**
 * The section switcher: a horizontal bar across the top of the shell.
 *
 * These are app sections, not a tab widget, so the markup is a plain `nav` of buttons with
 * `aria-current="page"` on the active one — the landmark and current-page attribute carry the
 * semantics a screen reader needs, without claiming tabpanel relationships the panels do not
 * have. The bar splits in two on a phone: the tabs move to a fixed bottom bar (`.bottom-nav`,
 * hidden on desktop) while the top bar keeps the brand, the account chip, and the theme toggle —
 * the thumb-reachable row for navigation, the eye-level row for identity.
 *
 * The account chip reads the signed-in account from the Migo context and opens the profile
 * section; the theme control is the shared {@link ThemeToggle}, overridable here for a parent (or
 * a test) that owns the theme state already.
 */
export function TopNav({
  active,
  onSelect,
  theme,
  onToggleTheme,
}: {
  /** The section currently shown; its button carries the current-page attribute. */
  active: AppTab;
  /** Called with the section a button click asks for. */
  onSelect: (tab: AppTab) => void;
  /** Pins the theme control's appearance; defaults to the persisted theme. */
  theme?: Theme;
  /** Called when the theme control is clicked; defaults to flipping the persisted theme. */
  onToggleTheme?: () => void;
}): ReactNode {
  const { accountId } = useMigo();
  const self = useProfile(accountId);

  return (
    <>
      <nav className="top-nav" aria-label="Sections">
        <div className="top-nav-brand">
          <span className="brand-mark" aria-hidden="true">
            ◆
          </span>
          <span className="brand-name">Migo</span>
        </div>
        <div className="top-nav-tabs">
          {TABS.map((tab) => (
            <button
              key={tab.id}
              type="button"
              className={`top-nav-tab ${active === tab.id ? 'active' : ''}`}
              aria-current={active === tab.id ? 'page' : undefined}
              onClick={() => onSelect(tab.id)}
            >
              <span className="top-nav-tab-icon" aria-hidden="true">
                {tab.icon}
              </span>
              <span className="top-nav-tab-label">{tab.label}</span>
            </button>
          ))}
        </div>
        <div className="top-nav-actions">
          {accountId !== null ? (
            <button
              type="button"
              className="top-nav-user"
              aria-label="Open your profile"
              title="Your profile"
              onClick={() => onSelect('profile')}
            >
              <Avatar
                name={self?.displayName ?? 'You'}
                id={accountId}
                size={26}
                avatarUrl={self?.avatarUrl}
              />
              <span className="top-nav-user-name">{self?.displayName ?? 'You'}</span>
            </button>
          ) : null}
          <ThemeToggle theme={theme} onToggle={onToggleTheme} />
        </div>
      </nav>
      <nav className="bottom-nav" aria-label="Sections">
        {TABS.map((tab) => (
          <button
            key={tab.id}
            type="button"
            className={`bottom-nav-btn ${active === tab.id ? 'active' : ''}`}
            aria-current={active === tab.id ? 'page' : undefined}
            aria-label={tab.label}
            onClick={() => onSelect(tab.id)}
          >
            <span className="bottom-nav-icon" aria-hidden="true">
              {tab.icon}
            </span>
            <span className="bottom-nav-label">{tab.label}</span>
          </button>
        ))}
      </nav>
    </>
  );
}
