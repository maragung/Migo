'use client';

/**
 * The Settings tab: the account's devices and its live sessions.
 *
 * Devices and sessions are two views of the same security question, and both are
 * server-owned facts — the lists, each revocation, and the bulk sign-out all ask the
 * server and re-read the result, because another device's login or logout is invisible
 * to local state. A device row is the account-root view (a device stays listed as
 * revoked after it is removed); a session row is the live view. Removing a device ends
 * every session on it, which is why it asks for confirmation first: it is the one
 * control here that signs somebody else out of the account entirely.
 *
 * The current session is identified by its own id (`grant.sessionId`), so it renders as
 * "This device" with no revoke control: the server refuses to let a session revoke
 * itself, and a button that always errors is a lie.
 *
 * The account's identity, email, passphrase, and key file live in the "My Account" panel
 * (account-panel.tsx), not here — Settings is the device and session security surface.
 *
 * The presentational halves are exported as controlled components over plain data, so the rules
 * (the current-session badge, the disabled self-revoke) are testable without a live client,
 * exactly like the other panels' extracted pieces.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import type { AccountSession, DeviceSummary, Id } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { getChoice, setChoice } from '@/lib/theme.js';
import type { ThemeChoice } from '@/lib/theme.js';
import { getChatTabsMode, setChatTabsMode } from '@/lib/chat-tabs-mode.js';
import type { ChatTabsMode } from '@/lib/chat-tabs-mode.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Spinner } from './spinner.js';

/** The presentational row for one of the account's devices. */
export function DeviceRowView({
  device,
  busy,
  onRemove,
}: {
  /** The wire row. */
  device: DeviceSummary;
  /** True while this row's removal is in flight. */
  busy: boolean;
  /**
   * Requests this device's removal (never called for the current device or an already
   * revoked one).
   */
  onRemove: (deviceId: Id) => void;
}): ReactNode {
  const revoked = device.status === 'revoked';
  return (
    <div className="person-row session-row">
      <div className="person-main">
        <span className="person-name">
          {device.displayName}
          {device.isCurrent ? <span className="tag tag-current">This device</span> : null}
          {revoked ? <span className="tag tag-revoked">Revoked</span> : null}
        </span>
        <span className="person-sub">
          {device.platform} · last seen {formatRelative(device.lastSeenAtMs)}
          {device.hasCredential ? ' · holds a sign-in credential' : ''}
        </span>
      </div>
      <div className="person-actions">
        {device.isCurrent || revoked ? null : (
          <button
            type="button"
            className="btn btn-ghost"
            disabled={busy}
            onClick={() => onRemove(device.deviceId)}
            aria-label={`Remove device ${device.displayName}`}
          >
            {busy ? <Spinner /> : 'Remove'}
          </button>
        )}
      </div>
    </div>
  );
}

/** The device list: every device the account knows, the current one marked. */
export function DeviceList({
  devices,
  busyId,
  onRemove,
}: {
  devices: DeviceSummary[];
  /** The device whose removal is in flight, so only its row shows the busy state. */
  busyId: Id | null;
  onRemove: (deviceId: Id) => void;
}): ReactNode {
  if (devices.length === 0) {
    return <p className="muted">No devices are registered.</p>;
  }
  return (
    <div className="session-list">
      {devices.map((device) => (
        <DeviceRowView
          key={device.deviceId}
          device={device}
          busy={busyId === device.deviceId}
          onRemove={onRemove}
        />
      ))}
    </div>
  );
}

/** The presentational row for one active session. */
export function SessionRow({
  session,
  current,
  busy,
  onRevoke,
}: {
  /** The wire row. */
  session: AccountSession;
  /** True when this row is the session doing the viewing. */
  current: boolean;
  /** True while this row's revoke is in flight. */
  busy: boolean;
  /** Requests this session's revocation (never called for the current session). */
  onRevoke: (sessionId: Id) => void;
}): ReactNode {
  return (
    <div className="person-row session-row">
      <div className="person-main">
        <span className="person-name">
          {session.device}
          {current ? <span className="tag tag-current">This device</span> : null}
        </span>
        <span className="person-sub">last active {formatRelative(session.last_seen_at)}</span>
      </div>
      <div className="person-actions">
        {current ? null : (
          <button
            type="button"
            className="btn btn-ghost"
            disabled={busy}
            onClick={() => onRevoke(session.id)}
            aria-label={`Revoke session on ${session.device}`}
          >
            {busy ? <Spinner /> : 'Revoke'}
          </button>
        )}
      </div>
    </div>
  );
}

/** The device list: one row per session, the current one marked, others revocable. */
export function SessionList({
  sessions,
  currentSessionId,
  busyId,
  onRevoke,
}: {
  sessions: AccountSession[];
  /** The viewing session's own id, so its row is marked and not revocable. */
  currentSessionId: Id | null;
  /** The session whose revoke is in flight, so only its row shows the busy state. */
  busyId: Id | null;
  onRevoke: (sessionId: Id) => void;
}): ReactNode {
  if (sessions.length === 0) {
    return <p className="muted">No active sessions.</p>;
  }
  return (
    <div className="session-list">
      {sessions.map((session) => (
        <SessionRow
          key={session.id}
          session={session}
          current={session.id === currentSessionId}
          busy={busyId === session.id}
          onRevoke={onRevoke}
        />
      ))}
    </div>
  );
}

/**
 * The Settings tab panel: loads the device and session lists.
 *
 * The Chats Tabs section's choice arrives from the shell when the shell is the one holding it
 * (switching modes reshapes the pane on the spot); rendered without the props — the extracted,
 * testable story — it holds the choice itself, backed by the same storage.
 */
export function SettingsPanel({
  chatTabsMode,
  onChatTabsMode,
}: {
  /** The display mode the shell is holding, when it is; omitted here means the section holds it. */
  chatTabsMode?: ChatTabsMode;
  /** The shell's write-through for a new choice, when there is one. */
  onChatTabsMode?: (mode: ChatTabsMode) => void;
}): ReactNode {
  const { client } = useMigo();

  const [devices, setDevices] = useState<DeviceSummary[] | null>(null);
  const [removing, setRemoving] = useState<Id | null>(null);
  const [sessions, setSessions] = useState<AccountSession[] | null>(null);
  const [revoking, setRevoking] = useState<Id | null>(null);
  const [signingOut, setSigningOut] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const currentSessionId = client?.grant.sessionId ?? null;

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const [deviceRows, sessionRows] = await Promise.all([client.devices(), client.sessions()]);
      setDevices(deviceRows);
      setSessions(sessionRows);
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const removeDevice = useCallback(
    (deviceId: Id): void => {
      if (!client || removing !== null) {
        return;
      }
      const device = devices?.find((row) => row.deviceId === deviceId);
      // Removing a device is the one control here that signs a device out of the account
      // entirely — every session on it ends and its credential stops working — so it is
      // never silent: the person names the device before the server acts.
      const named = device ? `“${device.displayName}”` : 'this device';
      const confirmed = window.confirm(
        `Remove ${named}? Every session on it will be signed out, and it will not be able ` +
          'to sign in with its credential again.',
      );
      if (!confirmed) {
        return;
      }
      setRemoving(deviceId);
      setNotice(null);
      client
        .revokeDevice({ device_id: deviceId })
        .then((result) => {
          setNotice(
            `Device removed; ${result.revoked} session${result.revoked === 1 ? '' : 's'} ended.`,
          );
          return reload();
        })
        .catch((cause: unknown) => {
          setError(friendlyError(cause));
        })
        .finally(() => {
          setRemoving(null);
        });
    },
    [client, devices, reload, removing],
  );

  const revoke = useCallback(
    (sessionId: Id): void => {
      if (!client || revoking !== null) {
        return;
      }
      setRevoking(sessionId);
      setNotice(null);
      client
        .revokeSession({ session_id: sessionId })
        .then(() => reload())
        .catch((cause: unknown) => {
          setError(friendlyError(cause));
        })
        .finally(() => {
          setRevoking(null);
        });
    },
    [client, revoking, reload],
  );

  const signOutOthers = useCallback((): void => {
    if (!client || signingOut) {
      return;
    }
    setSigningOut(true);
    setNotice(null);
    client
      .signOutOthers()
      .then((result) => {
        setNotice(`Signed out ${result.revoked} other session${result.revoked === 1 ? '' : 's'}.`);
        return reload();
      })
      .catch((cause: unknown) => {
        setError(friendlyError(cause));
      })
      .finally(() => {
        setSigningOut(false);
      });
  }, [client, signingOut, reload]);

  return (
    <div className="panel">
      <h1 className="panel-title">Settings</h1>

      {error ? <p className="form-error">{error}</p> : null}
      {notice ? <p className="hint">{notice}</p> : null}

      <section className="panel-section" aria-label="Devices">
        <h2 className="panel-heading">Devices</h2>
        <p className="hint">Every device that can sign in to your account.</p>
        {devices === null ? (
          <div className="center-fill">
            <Spinner />
          </div>
        ) : (
          <DeviceList devices={devices} busyId={removing} onRemove={removeDevice} />
        )}
      </section>

      <section className="panel-section" aria-label="Sessions">
        <h2 className="panel-heading">Sessions</h2>
        <p className="hint">Every session currently signed in to your account.</p>
        {sessions === null ? (
          <div className="center-fill">
            <Spinner />
          </div>
        ) : (
          <>
            <SessionList
              sessions={sessions}
              currentSessionId={currentSessionId}
              busyId={revoking}
              onRevoke={revoke}
            />
            <button
              type="button"
              className="btn btn-ghost"
              disabled={signingOut}
              onClick={signOutOthers}
            >
              {signingOut ? <Spinner /> : 'Sign out other devices'}
            </button>
          </>
        )}
      </section>

      <AppearanceSection />
      <ChatsTabsSection mode={chatTabsMode} onPick={onChatTabsMode} />
      <AboutSection />
    </div>
  );
}

/**
 * The Chats Tabs section: how the right pane holds the account's open chats.
 *
 * Two honest choices, each named for where the chats end up: "Right tabs" docks every open
 * chat as a closable chip in the right pane (the default — the side tabs don't need a Chats
 * section, the chips are the list), and "Chats list" puts Chats back among the side tabs,
 * drops the right pane's tab bar, and opens a chat as one full window at a time — back or
 * close returns to the list. Neither is a better way to chat; one is a better way to chat
 * for a given person, which is why it is a setting and not a decision.
 *
 * Exported as a controlled component over the shell's choice, so the rules (both options
 * offered, exactly one active, the active choice's story told) are testable without a shell.
 */
export function ChatsTabsSection({
  mode,
  onPick,
}: {
  /** The mode the shell is holding, when it is; omitted here means this section holds it. */
  mode?: ChatTabsMode;
  /** The shell's write-through for a new choice, when there is one. */
  onPick?: (mode: ChatTabsMode) => void;
}): ReactNode {
  const [held, setHeld] = useState<ChatTabsMode>(() => getChatTabsMode());
  const current = mode ?? held;
  function pick(next: ChatTabsMode): void {
    setChatTabsMode(next);
    if (onPick !== undefined) {
      onPick(next);
      return;
    }
    setHeld(next);
  }
  const options: ReadonlyArray<{ id: ChatTabsMode; label: string; blurb: string }> = [
    {
      id: 'right',
      label: 'Right tabs',
      blurb:
        'Every open chat docks as a closable tab in the right pane, one beside another — the side tabs drop their Chats section.',
    },
    {
      id: 'list',
      label: 'Chats list',
      blurb:
        'Chats stays with the side tabs, the right pane drops its tab bar, and a chat opens as one full window at a time — close takes you back to the list.',
    },
  ];
  const active = options.find((option) => option.id === current);
  return (
    <section className="panel-section" aria-label="Chats Tabs">
      <h2 className="panel-heading">Chats Tabs</h2>
      <div className="chip-row" role="group" aria-label="Chats Tabs">
        {options.map((option) => (
          <button
            key={option.id}
            type="button"
            className={`chip ${current === option.id ? 'chip-active' : ''}`}
            aria-pressed={current === option.id}
            onClick={() => pick(option.id)}
          >
            {option.label}
          </button>
        ))}
      </div>
      <p className="muted">{active?.blurb}</p>
    </section>
  );
}

/**
 * The Appearance section: the theme, stated as three named choices rather than a toggle.
 *
 * System is offered first because it is the choice that keeps itself correct; light and dark
 * exist for the times a room's lighting argues with the OS.
 */
function AppearanceSection(): ReactNode {
  const [choice, setChoiceState] = useState<ThemeChoice>(() => getChoice());
  function pick(next: ThemeChoice): void {
    setChoice(next);
    setChoiceState(next);
  }
  return (
    <section className="panel-section" aria-label="Appearance">
      <h2 className="panel-heading">Appearance</h2>
      <div className="chip-row" role="group" aria-label="Theme">
        {(['system', 'dark', 'light'] as const).map((option) => (
          <button
            key={option}
            type="button"
            className={`chip ${choice === option ? 'chip-active' : ''}`}
            aria-pressed={choice === option}
            onClick={() => pick(option)}
          >
            {option.charAt(0).toUpperCase() + option.slice(1)}
          </button>
        ))}
      </div>
      <p className="muted">System follows this device's colour scheme; dark is Migo's home skin.</p>
    </section>
  );
}

/** The About section: what this build is, and the door to the design system. */
function AboutSection(): ReactNode {
  return (
    <section className="panel-section" aria-label="About">
      <h2 className="panel-heading">About</h2>
      <p className="muted">
        Migo — compact, social, realtime. One design system across every screen size.
      </p>
      <a className="btn btn-ghost" href="/design/">
        Design system
      </a>
    </section>
  );
}
