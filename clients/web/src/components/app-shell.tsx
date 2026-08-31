'use client';

/**
 * The messenger shell: Migo's primary navigation, re-composed per size.
 *
 * The spec is explicit about what Migo is NOT: a SaaS dashboard with a large permanent sidebar
 * and an empty content area. It is a messenger, so the shell leads with people — FRIENDS |
 * CHATS | ROOMS as a trio of tabs (mobile: under the header; tablet/desktop: an icon rail with
 * the trio prominent above the secondary destinations) — and every other destination is one
 * compact control away. The rail stays an *icon* rail at every width: 56px of navigation, never
 * a 240px column, so the conversations — the thing the product is — keep the screen.
 *
 * The trio is one tab group for a reason: Friends, Chats, and Rooms are the three lists a
 * messenger lives in, and switching between them should feel like paging through one surface,
 * not navigating between sections. Everything else (Space, Alerts, Search, Wallet, Profile,
 * Settings) is a destination, not a list, and lives below the fold of the rail.
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
 * The app's top-level sections.
 *
 * The order is the messenger's own: the three lists first (friends, chats, rooms), then the
 * stream, then the destinations. `trio` marks the three that form the primary tab group.
 */
export type AppTab =
  | 'friends'
  | 'chats'
  | 'rooms'
  | 'space'
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

/** The primary trio: the three lists a messenger lives in. */
const TRIO: ReadonlyArray<Section> = [
  { id: 'friends', label: 'Friends', icon: 'friends' },
  { id: 'chats', label: 'Chats', icon: 'chats' },
  { id: 'rooms', label: 'Rooms', icon: 'rooms' },
];

/** The rest: the stream and the destinations, below the fold of the rail. */
const REST: ReadonlyArray<Section> = [
  { id: 'space', label: 'Space', icon: 'space' },
  { id: 'notifications', label: 'Alerts', icon: 'bell' },
  { id: 'search', label: 'Search', icon: 'search' },
  { id: 'wallet', label: 'Wallet', icon: 'wallet' },
];

/** The rail's foot: about the account, not destinations. */
const SECONDARY: ReadonlyArray<Section> = [
  { id: 'profile', label: 'Profile', icon: 'user' },
  { id: 'settings', label: 'Settings', icon: 'settings' },
];

/** The mobile bar: the trio plus More — five slots, the spec's ceiling. */
const MOBILE_BAR: ReadonlyArray<Section> = [
  ...TRIO,
  { id: 'space', label: 'Space', icon: 'space' },
];

/**
 * The shell around every authenticated screen.
 *
 * @param active The section currently shown; its control carries the current-page attribute.
 * @param onSelect Called with the section a control asks for.
 * @param hasThread True while a chat thread is open — the mobile chrome folds away for it.
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
      {/* The tablet/desktop icon rail: the trio prominent, the rest below the fold. */}
      <aside className="rail" aria-label="Sections">
        <button
          type="button"
          className="rail-btn rail-btn-brand"
          onClick={() => select('chats')}
          title="Migo"
          aria-label="Migo — Chats"
        >
          <span className="rail-btn-icon" aria-hidden="true">
            <Icon name="sparkle" size={20} />
          </span>
          <span className="rail-btn-label">Migo</span>
        </button>
        <div className="rail-trio" role="tablist" aria-label="Lists">
          {TRIO.map((section) => (
            <RailButton
              key={section.id}
              section={section}
              active={active === section.id}
              onSelect={select}
            />
          ))}
        </div>
        <div className="rail-divider" aria-hidden="true" />
        <nav className="rail-nav">
          {REST.map((section) => (
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

      {/* The mobile bar: the trio plus Space — the messenger's five slots. */}
      <nav className="bottom-nav" aria-label="Sections">
        {MOBILE_BAR.map((section) => (
          <button
            key={section.id}
            type="button"
            className={`bottom-nav-btn ${active === section.id ? 'active' : ''}`}
            aria-current={active === section.id ? 'page' : undefined}
            onClick={() => select(section.id)}
          >
            <span className="bottom-nav-icon" aria-hidden="true">
              <Icon name={section.icon} size={20} />
            </span>
            <span className="bottom-nav-label">{section.label}</span>
          </button>
        ))}
        <button
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
      </nav>

      {/* The More sheet: every section the five-slot bar could not carry. */}
      {moreOpen ? (
        <BottomSheet title="More" onClose={() => setMoreOpen(false)}>
          <div className="sheet-menu">
            {[...REST.slice(1), ...SECONDARY].map((section) => (
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

/** One rail entry: icon always, label on hover for the finding, tooltip always. */
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
      aria-label={section.label}
      onClick={() => onSelect(section.id)}
    >
      <span className="rail-btn-icon" aria-hidden="true">
        <Icon name={section.icon} size={20} />
      </span>
      <span className="rail-btn-label">{section.label}</span>
    </button>
  );
}
