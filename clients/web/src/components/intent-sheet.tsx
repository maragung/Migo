'use client';

/**
 * The mobile intent modals: bottom sheets that ask what a tap meant.
 *
 * A phone has no room for a persistent userlist beside every list, so the reference design
 * makes every person and room a *target*: tapping one opens a sheet that offers the actions
 * (the intents) the wire actually carries for that target — send a message, place a call, join
 * a room. The actions are the caller's to perform; the sheet is presentation, so the same
 * surface serves the home lists and any other list that grows intents later.
 *
 * The sheet itself (backdrop, drag handle, title, close) is the design's own: it rises from the
 * bottom edge, dismisses on backdrop tap or Escape, and respects the bottom safe area.
 */

import { useEffect } from 'react';
import type { ReactNode } from 'react';
import { createPortal } from 'react-dom';

import { PresenceState } from '@migo/sdk';
import type { Id, PresenceState as PresenceStateValue, RoomSummary } from '@migo/sdk';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import type { IconName } from './icons.js';

/** The ink of a presence dot, as a CSS colour the pill and the header share. */
export const presenceColor = (state: PresenceStateValue | undefined): string => {
  switch (state) {
    case PresenceState.Online:
      return 'var(--dot-online)';
    case PresenceState.Away:
      return 'var(--warning)';
    case PresenceState.Busy:
      return 'var(--danger)';
    default:
      return 'var(--dot-offline)';
  }
};

/** A short label for a presence state. */
export const presenceName = (state: PresenceStateValue | undefined): string => {
  switch (state) {
    case PresenceState.Online:
      return 'Online';
    case PresenceState.Away:
      return 'Away';
    case PresenceState.Busy:
      return 'Busy';
    case PresenceState.Invisible:
      return 'Invisible';
    default:
      return 'Offline';
  }
};

/** The generic bottom sheet: backdrop, handle, title, close — and nothing else. */
export function Sheet({
  open,
  onClose,
  title,
  children,
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  children: ReactNode;
}): ReactNode {
  useEffect(() => {
    if (!open) {
      return;
    }
    function onKey(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        onClose();
      }
    }
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
    };
  }, [open, onClose]);

  if (!open) {
    return null;
  }
  return createPortal(
    <>
      <div className="sheet-backdrop" onClick={onClose} aria-hidden="true" />
      <div
        className="sheet"
        role="dialog"
        aria-modal="true"
        aria-label={typeof title === 'string' ? title : 'Sheet'}
      >
        <div className="sheet-handle" aria-hidden="true" />
        <div className="sheet-head">
          <span className="sheet-title">{title}</span>
          <button type="button" className="sheet-x" onClick={onClose} aria-label="Close">
            <Icon name="close" size={16} />
          </button>
        </div>
        <div className="sheet-body retro-scroll">{children}</div>
      </div>
    </>,
    document.body,
  );
}

/** One tappable action row: icon chip, label, optional sub-line, chevron — or the orange CTA. */
export function SheetAction({
  icon,
  label,
  sub,
  danger = false,
  primary = false,
  onClick,
}: {
  icon: IconName;
  label: string;
  sub?: string;
  danger?: boolean;
  primary?: boolean;
  onClick: () => void;
}): ReactNode {
  if (primary) {
    return (
      <button type="button" className="sheet-action-primary" onClick={onClick}>
        <Icon name={icon} size={19} />
        {label}
      </button>
    );
  }
  return (
    <button
      type="button"
      className={`sheet-action${danger ? ' sheet-action-danger' : ''}`}
      onClick={onClick}
    >
      <span className={`sheet-action-ico${danger ? ' sheet-action-ico-danger' : ''}`}>
        <Icon name={icon} size={19} />
      </span>
      <span className="sheet-action-text">
        <span className="sheet-action-label">{label}</span>
        {sub !== undefined ? <span className="sheet-action-sub">{sub}</span> : null}
      </span>
      <Icon name="chevron-right" size={18} className="sheet-action-go" />
    </button>
  );
}

/** A presence choice in the me sheet: dot, name, and a check when it is the current one. */
export function PresencePill({
  state,
  current,
  onPick,
}: {
  state: PresenceStateValue;
  current: PresenceStateValue;
  onPick: (next: PresenceStateValue) => void;
}): ReactNode {
  const on = state === current;
  return (
    <button
      type="button"
      className={`presence-pill${on ? ' presence-pill-on' : ''}`}
      onClick={() => onPick(state)}
      aria-pressed={on}
    >
      <span className="presence-pill-dot" style={{ background: presenceColor(state) }} />
      {presenceName(state)}
      {on ? <Icon name="check" size={15} className="presence-pill-check" /> : null}
    </button>
  );
}

/** The user sheet: who they are, then what you can do with them. */
export function UserIntentSheet({
  target,
  name,
  username,
  status,
  presence,
  avatarUrl,
  isFriend,
  onClose,
  onSend,
  onCall,
  onAddFriend,
}: {
  /** The person the sheet is about; `null` keeps the component mounted but closed. */
  target: Id | null;
  name: string;
  username?: string;
  status?: string;
  presence?: PresenceStateValue;
  avatarUrl?: string;
  isFriend: boolean;
  onClose: () => void;
  /** Opens (or creates) the direct conversation with the person. */
  onSend: () => void;
  /** Places a 1:1 call; the boolean says voice or video. */
  onCall: (video: boolean) => void;
  /** Sends a friend request; shown only for someone who is not a friend yet. */
  onAddFriend?: () => void;
}): ReactNode {
  const open = target !== null;
  return (
    <Sheet open={open} onClose={onClose} title={open ? name : ''}>
      {open ? (
        <>
          <div className="sheet-target">
            <span className="sheet-target-avatar">
              <Avatar name={name} id={target ?? 'user'} size={48} avatarUrl={avatarUrl} />
              <span
                className="sheet-target-dot"
                style={{ background: presenceColor(presence) }}
                aria-hidden="true"
              />
            </span>
            <span className="sheet-target-main">
              <span className="sheet-target-name">{name}</span>
              {username !== undefined && username.length > 0 ? (
                <span className="sheet-target-sub">@{username}</span>
              ) : null}
              <span className="sheet-target-sub">
                <span
                  className="sheet-target-presence"
                  style={{ background: presenceColor(presence) }}
                  aria-hidden="true"
                />
                {isFriend ? presenceName(presence) : 'Migo member'}
              </span>
              {status !== undefined && status.length > 0 ? (
                <span className="sheet-target-status">“{status}”</span>
              ) : null}
            </span>
          </div>
          <SheetAction
            primary
            icon="send"
            label="Send message"
            sub={isFriend ? `Chat privately with ${name}` : undefined}
            onClick={onSend}
          />
          <div className="sheet-action-grid">
            <SheetAction icon="phone" label="Voice call" onClick={() => onCall(false)} />
            <SheetAction icon="video" label="Video call" onClick={() => onCall(true)} />
          </div>
          {!isFriend && onAddFriend !== undefined ? (
            <SheetAction icon="user-plus" label="Add to friends" onClick={onAddFriend} />
          ) : null}
          <div className="sheet-tail" />
        </>
      ) : null}
    </Sheet>
  );
}

/** The room sheet: how full it is, then the way in. */
export function RoomIntentSheet({
  room,
  online,
  capacity,
  joined,
  onClose,
  onJoin,
}: {
  /** The room the sheet is about; `null` keeps the component mounted but closed. */
  room: RoomSummary | null;
  /** The live online count, when the shell holds one for this room. */
  online?: number;
  /** The room's ceiling, when the wire states one. */
  capacity?: number;
  joined: boolean;
  onClose: () => void;
  onJoin: () => void;
}): ReactNode {
  const open = room !== null;
  const users = room !== null ? (online ?? room.onlineCount ?? 0) : 0;
  const max = room !== null ? (capacity ?? room.maxMembers ?? 0) : 0;
  const pct = max > 0 ? Math.min(100, Math.round((users / max) * 100)) : 0;
  const nearFull = pct >= 85;
  return (
    <Sheet open={open} onClose={onClose} title={open ? (room?.name ?? '') : ''}>
      {open && room !== null ? (
        <>
          <div className="sheet-target">
            <span className="part-chip sheet-room-chip">
              <Icon name="rooms" size={26} />
            </span>
            <span className="sheet-target-main">
              <span className="sheet-target-name-row">
                <span className="sheet-target-name">{room.name}</span>
                {max > 0 ? (
                  <span className={`room-count${nearFull ? ' room-count-full' : ''}`}>
                    <b>{users}</b>/{max}
                  </span>
                ) : null}
              </span>
              <span className="sheet-target-sub">
                {room.topic ?? room.description ?? 'A public room'}
              </span>
              {max > 0 ? (
                <span className="sheet-occupancy" aria-hidden="true">
                  <span
                    style={{
                      width: `${pct}%`,
                      background: nearFull ? 'var(--migo-orange)' : 'var(--accent)',
                    }}
                  />
                </span>
              ) : null}
            </span>
          </div>
          <SheetAction
            primary
            icon={joined ? 'rooms' : 'plus'}
            label={joined ? 'Open room' : 'Join room'}
            sub={
              joined
                ? 'You are already in this room'
                : `${users.toLocaleString()} ${users === 1 ? 'person is' : 'people are'} inside right now`
            }
            onClick={onJoin}
          />
          <div className="sheet-tail" />
        </>
      ) : null}
    </Sheet>
  );
}
