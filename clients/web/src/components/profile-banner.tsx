'use client';

/**
 * The profile banner: the orange strip under the tabs that owns the account.
 *
 * The reference draws an orange-to-amber gradient carrying the avatar, the display name with a
 * presence dot, a mood line, and a counter chip. The real banner keeps the shape and swaps the
 * mockup's egg count for the account's honest $MIG balance (one read per session, the same
 * posture as the sidebar's coin badge), and the avatar becomes the menu the rail's foot used to
 * be: My Profile, My Credits & TopUp, Settings, and Exit/Logout — the panels that have no
 * system tab of their own.
 */

import { useEffect, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { PresenceState } from '@migo/sdk';
import type { PresenceState as PresenceStateValue } from '@migo/sdk';

import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfile } from '@/lib/migo/use-profiles.js';
import type { Theme } from '@/lib/theme.js';

import { Avatar } from './avatar.js';
import { CoinMark, Icon } from './icons.js';
import { PresenceSelect, StatusInput } from './presence-picker.js';
import { ThemeToggle } from './theme-toggle.js';
import type { PanelTab } from './tab-strip.js';

/** The banner's menu: where the account's own surfaces open from. */
const MENU: ReadonlyArray<{
  panel: PanelTab;
  label: string;
  icon: 'bell' | 'search' | 'user' | 'wallet' | 'settings' | 'shield';
}> = [
  { panel: 'account', label: 'My Account', icon: 'shield' },
  { panel: 'profile', label: 'My Profile', icon: 'user' },
  { panel: 'wallet', label: 'My Credits & TopUp', icon: 'wallet' },
  { panel: 'notifications', label: 'Alerts', icon: 'bell' },
  { panel: 'search', label: 'Search', icon: 'search' },
  { panel: 'settings', label: 'Settings', icon: 'settings' },
];

/**
 * The owner-only entry, rendered apart from the everyday menu because its
 * visibility is a server answer, not a build constant: the deployment's
 * Owner/CEO is named in configuration, and a client that asked nothing would
 * either show every account a tab that only errors or hide it from the one
 * account that needs it.
 */
const OWNER_ENTRY: {
  panel: PanelTab;
  label: string;
  icon: 'shield';
} = { panel: 'admins', label: 'Global Admins', icon: 'shield' };

/**
 * The presence dot's colour class per state: the numeric enum does not stringify into a name,
 * so the mapping is explicit — one glance at the banner says the state the dropdown will show.
 */
const DOT_CLASS: Readonly<Record<PresenceStateValue, string>> = {
  [PresenceState.Unknown]: 'offline',
  [PresenceState.Offline]: 'offline',
  [PresenceState.Online]: 'online',
  [PresenceState.Away]: 'away',
  [PresenceState.Busy]: 'busy',
  [PresenceState.Invisible]: 'invisible',
};

/**
 * @param onOpenPanel Opens a secondary panel as a tab — what every menu entry does.
 * @param theme Pins the theme control's appearance; defaults to the persisted theme.
 * @param onToggleTheme Called when the theme control is clicked.
 */
export function ProfileBanner({
  onOpenPanel,
  theme,
  onToggleTheme,
}: {
  onOpenPanel: (panel: PanelTab) => void;
  theme?: Theme;
  onToggleTheme?: () => void;
}): ReactNode {
  const { client, accountId, logout } = useMigo();
  const self = useProfile(accountId);
  const [menuOpen, setMenuOpen] = useState(false);
  const [coins, setCoins] = useState<number | null>(null);
  const [owner, setOwner] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);
  // The account's own presence, published from this banner: the state rides beside the coin
  // chip, the status line under the @username, and both are publishes, not reads — the banner
  // holds the state and performs the call, never optimistically keeping a change the server
  // refused.
  const [myPresence, setMyPresence] = useState<PresenceStateValue>(PresenceState.Online);
  const [myStatus, setMyStatus] = useState('');
  const seededStatus = useRef(false);

  // The status the profile already carries seeds the input once: a returning session says what
  // it said yesterday rather than offering an empty box next to a profile that plainly has one.
  useEffect(() => {
    if (seededStatus.current || !self) {
      return;
    }
    seededStatus.current = true;
    setMyStatus(self.customStatus ?? '');
  }, [self]);

  /** Publishes the presence and status together — the wire carries them as one call. */
  function publish(state: PresenceStateValue, status: string): void {
    setMyPresence(state);
    setMyStatus(status);
    if (!client) {
      return;
    }
    void client.presence
      .setPresence(state, status.trim().length > 0 ? { customStatus: status } : {})
      .catch(() => {});
  }

  // The admin entry appears only for the account the deployment names as its
  // Owner/CEO. One read per session, absent on failure rather than wrong — a
  // client that cannot ask is a client that shows nothing it cannot stand behind.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client
      .adminStanding()
      .then((standing) => {
        if (!cancelled) {
          setOwner(standing.owner);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client]);

  // The balance chip is the resting glance, not the ledger: one read per session, absent on
  // failure rather than wrong — the same contract the sidebar's coin badge keeps.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client.economy
      .getBalance()
      .then((wallet) => {
        if (!cancelled) {
          setCoins(wallet.balance);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client]);

  // A click outside the dropdown closes it; the menu is a menu, not a mode.
  useEffect(() => {
    if (!menuOpen) {
      return;
    }
    function onPointerDown(event: PointerEvent): void {
      if (menuRef.current !== null && !menuRef.current.contains(event.target as Node)) {
        setMenuOpen(false);
      }
    }
    document.addEventListener('pointerdown', onPointerDown);
    return () => document.removeEventListener('pointerdown', onPointerDown);
  }, [menuOpen]);

  return (
    <header className="profile-banner">
      <div className="banner-menu-anchor" ref={menuRef}>
        <button
          type="button"
          className="banner-avatar"
          aria-haspopup="menu"
          aria-expanded={menuOpen}
          aria-label="Open the account menu"
          title="Account menu"
          onClick={() => setMenuOpen((open) => !open)}
        >
          <Avatar
            name={self?.displayName ?? 'You'}
            id={accountId ?? 'self'}
            size={32}
            avatarUrl={self?.avatarUrl}
          />
        </button>

        {menuOpen ? (
          <div className="banner-menu" role="menu">
            <div className="banner-menu-head">
              <p className="banner-menu-name">{self?.displayName ?? 'You'}</p>
              <p className="banner-menu-sub">
                {self?.username ? `@${self.username}` : 'Signed in'}
              </p>
            </div>
            {MENU.map((entry) => (
              <button
                key={entry.panel}
                type="button"
                role="menuitem"
                className="banner-menu-item"
                onClick={() => {
                  setMenuOpen(false);
                  onOpenPanel(entry.panel);
                }}
              >
                <Icon name={entry.icon} size={16} />
                <span>{entry.label}</span>
              </button>
            ))}
            {owner ? (
              <button
                type="button"
                role="menuitem"
                className="banner-menu-item"
                onClick={() => {
                  setMenuOpen(false);
                  onOpenPanel(OWNER_ENTRY.panel);
                }}
              >
                <Icon name={OWNER_ENTRY.icon} size={16} />
                <span>{OWNER_ENTRY.label}</span>
              </button>
            ) : null}
            <div className="banner-menu-divider" aria-hidden="true" />
            <button
              type="button"
              role="menuitem"
              className="banner-menu-item banner-menu-signout"
              onClick={() => {
                setMenuOpen(false);
                void logout();
              }}
            >
              <Icon name="signout" size={16} />
              <span>Exit / Logout</span>
            </button>
          </div>
        ) : null}
      </div>

      <div className="banner-id">
        <span className="banner-name">
          <span
            className={`banner-presence banner-presence-${DOT_CLASS[myPresence]}`}
            aria-hidden="true"
          />
          {self?.displayName ?? 'You'}
        </span>
        <span className="banner-status">
          {self?.username ? `@${self.username}` : 'End-to-end encrypted'}
        </span>
        <StatusInput state={myPresence} status={myStatus} onChange={publish} />
      </div>

      <div className="banner-actions">
        <PresenceSelect state={myPresence} onStateChange={(next) => publish(next, myStatus)} />
        {coins !== null ? (
          <span className="banner-chip" title="Coin balance" aria-label={`Coin balance: ${coins}`}>
            <CoinMark size={14} />
            <span>{coins.toLocaleString()}</span>
          </span>
        ) : null}
        <ThemeToggle theme={theme} onToggle={onToggleTheme} />
      </div>
    </header>
  );
}
