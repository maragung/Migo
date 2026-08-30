'use client';

/**
 * The app shell: one product, three compositions.
 *
 * The sections are the app's whole information architecture — Home, Chats, Rooms, Space, then
 * the rest — and the shell presents exactly that list at every size, only re-composed:
 *
 *   - Desktop (≥1024px): a full rail (icons + labels) beside the content.
 *   - Tablet (768–1023px): the same rail collapsed to icons, so the content keeps the room.
 *   - Mobile (<768px): a compact 44px header, the content, and a five-slot bottom bar — Home,
 *     Chats, Rooms, Space, and More — with More opening a bottom sheet that carries the
 *     remaining sections. Five is the ceiling: a bottom bar that scrolls is a bottom bar that
 *     hides.
 *
 * Section state stays plain client state (the bundle is a static export; the open conversation
 * lives in the URL fragment; a section switch must never unload the session or the socket).
 * While a chat thread is open on mobile the header folds away — the thread's own header, with
 * its back button, is the only chrome that pane needs.
 */

import { useState } from 'react';
import type { ReactNode } from 'react';

import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfile } from '@/lib/migo/use-profiles.js';
import type { Theme } from '@/lib/theme.js';

import { Avatar } from './avatar.js';
import { BottomSheet } from './bottom-sheet.js';
import { Icon } from './icons.js';
import type { IconName } from './icons.js';
import { ThemeToggle } from './theme-toggle.js';

/**
 * The app's top-level sections, switched by the shell's navigation.
 *
 * The order is the information architecture: the realtime surfaces first (Home summarises them,
 * Chats and Rooms are where talking happens, Space is the activity stream), people next, tools
 * last. `AppTab` is exported for the layout that owns the state and any test that pins the offer.
 */
export type AppTab =
  | 'home'
  | 'chats'
  | 'rooms'
  | 'space'
  | 'friends'
  | 'notifications'
  | 'search'
  | 'wallet'
  | 'profile'
  | 'settings';

/** A section as the navigation knows it: where it goes, what it is called, what it is drawn with. */
interface Section {
  id: AppTab;
  label: string;
  icon: IconName;
}

/** The rail's full list, in information-architecture order. */
const SECTIONS: ReadonlyArray<Section> = [
  { id: 'home', label: 'Home', icon: 'home' },
  { id: 'chats', label: 'Chats', icon: 'chats' },
  { id: 'rooms', label: 'Rooms', icon: 'rooms' },
  { id: 'space', label: 'Space', icon: 'space' },
  { id: 'friends', label: 'Friends', icon: 'friends' },
  { id: 'notifications', label: 'Alerts', icon: 'bell' },
  { id: 'search', label: 'Search', icon: 'search' },
  { id: 'wallet', label: 'Wallet', icon: 'wallet' },
];

/** The secondary row at the rail's foot: about the account, not destinations. */
const SECONDARY: ReadonlyArray<Section> = [
  { id: 'profile', label: 'Profile', icon: 'user' },
  { id: 'settings', label: 'Settings', icon: 'settings' },
];

/** The five-slot mobile bar; `more` is not a section but the sheet that carries the rest. */
const MOBILE_BAR: ReadonlyArray<Section | { id: 'more'; label: string; icon: IconName }> = [
  { id: 'home', label: 'Home', icon: 'home' },
  { id: 'chats', label: 'Chats', icon: 'chats' },
  { id: 'rooms', label: 'Rooms', icon: 'rooms' },
  { id: 'space', label: 'Space', icon: 'space' },
  { id: 'more', label: 'More', icon: 'menu' },
];

/**
 * The shell around every authenticated screen.
 *
 * @param active The section currently shown; its control carries the current-page attribute.
 * @param onSelect Called with the section a control asks for.
 * @param hasThread True while a chat thread is open — the mobile header folds away for it.
 * @param children The section's content, already chosen by the owner.
 */
export function AppShell({
  active,
  onSelect,
  hasThread = false,
  children,
  theme,
  onToggleTheme,
}: {
  active: AppTab;
  onSelect: (tab: AppTab) => void;
  hasThread?: boolean;
  children: ReactNode;
  /** Pins the theme control's appearance; defaults to the persisted theme. */
  theme?: Theme;
  /** Called when the theme control is clicked; defaults to flipping the persisted theme. */
  onToggleTheme?: () => void;
}): ReactNode {
  const { accountId } = useMigo();
  const self = useProfile(accountId);
  const [moreOpen, setMoreOpen] = useState(false);

  function select(tab: AppTab): void {
    setMoreOpen(false);
    onSelect(tab);
  }

  return (
    <div className={`app app-${active}${hasThread ? ' app-thread-open' : ''}`}>
      {/* The tablet/desktop rail: brand, the sections, the account. */}
      <aside className="rail" aria-label="Sections">
        <div className="rail-brand">
          <span className="brand-mark" aria-hidden="true">
            <Icon name="sparkle" size={20} />
          </span>
          <span className="brand-name rail-brand-name">Migo</span>
        </div>
        <nav className="rail-nav">
          {SECTIONS.map((section) => (
            <RailButton
              key={section.id}
              section={section}
              active={active === section.id}
              onSelect={select}
            />
          ))}
        </nav>
        <div className="rail-foot">
          {SECONDARY.map((section) => (
            <RailButton
              key={section.id}
              section={section}
              active={active === section.id}
              onSelect={select}
            />
          ))}
          {accountId !== null ? (
            <button
              type="button"
              className={`rail-user ${active === 'profile' ? 'active' : ''}`}
              onClick={() => select('profile')}
              aria-current={active === 'profile' ? 'page' : undefined}
            >
              <Avatar
                name={self?.displayName ?? 'You'}
                id={accountId}
                size={26}
                avatarUrl={self?.avatarUrl}
              />
              <span className="rail-user-name">{self?.displayName ?? 'You'}</span>
            </button>
          ) : null}
        </div>
      </aside>

      {/* The mobile header: identity and the two controls that are not sections. */}
      <header className="mobile-header">
        <span className="brand-mark" aria-hidden="true">
          <Icon name="sparkle" size={20} />
        </span>
        <span className="brand-name mobile-brand-name">Migo</span>
        <div className="mobile-header-actions">
          <ThemeToggle theme={theme} onToggle={onToggleTheme} />
          {accountId !== null ? (
            <button
              type="button"
              className="mobile-header-avatar"
              aria-label="Open your profile"
              onClick={() => select('profile')}
            >
              <Avatar
                name={self?.displayName ?? 'You'}
                id={accountId}
                size={26}
                avatarUrl={self?.avatarUrl}
              />
            </button>
          ) : null}
        </div>
      </header>

      {/* The section content. */}
      <div className="app-body">{children}</div>

      {/* The mobile bar: five slots, thumb reach, no scrolling. */}
      <nav className="bottom-nav" aria-label="Sections">
        {MOBILE_BAR.map((item) =>
          item.id === 'more' ? (
            <button
              key="more"
              type="button"
              className={`bottom-nav-btn ${moreOpen ? 'active' : ''}`}
              aria-haspopup="dialog"
              aria-expanded={moreOpen}
              onClick={() => setMoreOpen(true)}
            >
              <span className="bottom-nav-icon" aria-hidden="true">
                <Icon name="menu" size={20} />
              </span>
              <span className="bottom-nav-label">More</span>
            </button>
          ) : (
            <button
              key={item.id}
              type="button"
              className={`bottom-nav-btn ${active === item.id ? 'active' : ''}`}
              aria-current={active === item.id ? 'page' : undefined}
              onClick={() => select(item.id)}
            >
              <span className="bottom-nav-icon" aria-hidden="true">
                <Icon name={item.icon} size={20} />
              </span>
              <span className="bottom-nav-label">{item.label}</span>
            </button>
          ),
        )}
      </nav>

      {/* The More sheet: every section the five-slot bar could not carry. */}
      {moreOpen ? (
        <BottomSheet title="More" onClose={() => setMoreOpen(false)}>
          <div className="sheet-menu">
            {[...SECTIONS.slice(4), ...SECONDARY].map((section) => (
              <button
                key={section.id}
                type="button"
                className={`sheet-menu-btn ${active === section.id ? 'active' : ''}`}
                onClick={() => select(section.id)}
              >
                <span className="sheet-menu-icon" aria-hidden="true">
                  <Icon name={section.icon} size={20} />
                </span>
                <span>{section.label}</span>
                {active === section.id ? (
                  <span className="sheet-menu-current" aria-hidden="true">
                    <Icon name="chevron-right" size={16} />
                  </span>
                ) : null}
              </button>
            ))}
          </div>
        </BottomSheet>
      ) : null}
    </div>
  );
}

/** One rail entry: icon always, label beside it where the rail has room. */
function RailButton({
  section,
  active,
  onSelect,
}: {
  section: Section;
  active: boolean;
  onSelect: (tab: AppTab) => void;
}): ReactNode {
  return (
    <button
      type="button"
      className={`rail-btn ${active ? 'active' : ''}`}
      aria-current={active ? 'page' : undefined}
      title={section.label}
      onClick={() => onSelect(section.id)}
    >
      <span className="rail-btn-icon" aria-hidden="true">
        <Icon name={section.icon} size={20} />
      </span>
      <span className="rail-btn-label">{section.label}</span>
    </button>
  );
}
