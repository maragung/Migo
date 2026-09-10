'use client';

/**
 * The Settings tab, grouped by what a setting is about rather than by the screen it grew up on.
 *
 * The groups — and their headings are the same words the Android and desktop clients use, so a
 * person who looks for a control in one client knows what to look for in the others:
 *
 *   * **Chats & Log** — the chat-log exports: per-conversation download lives in the thread's
 *     header, and here the auto-save toggle, the saved snapshots, and the export-everything
 *     walk live together. The group carries the one-line honesty a plaintext artifact deserves:
 *     logs are decrypted content, stored only on this device.
 *   * **Privasi & Keamanan** — the account's devices and live sessions, and the door to the
 *     Security Checkup (§50). These are the server-owned security facts; the lists, each
 *     revocation, and the bulk sign-out all ask the server and re-read the result, because
 *     another device's login or logout is invisible to local state.
 *   * **Penyimpanan & Data** — what this device is holding for the app (the storage estimate)
 *     and the one honest broom: clearing the saved chat logs, which names exactly what it keeps.
 *   * **Tampilan** — the theme.
 *   * **Akun** — the door to the account panel (identity, email, passphrase, key file).
 *
 * There is deliberately no notification group: this client renders its alerts in its own panel
 * and never asks the OS to show them, and a settings group is not the place to invent a
 * notification system.
 *
 * The current session is identified by its own id (`grant.sessionId`), so it renders as
 * "This device" with no revoke control: the server refuses to let a session revoke itself, and a
 * button that always errors is a lie.
 *
 * The presentational halves are exported as controlled components over plain data, so the rules
 * (the current-session badge, the disabled self-revoke) are testable without a live client,
 * exactly like the other panels' extracted pieces.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import type { AccountSession, DeviceSummary, Id } from '@migo/sdk';

import { formatBytes, formatRelative } from '@/lib/format.js';
import { downloadTextFile, formatTranscriptText, logFileName } from '@/lib/chat-logs.js';
import type { StoredChatLog } from '@/lib/chat-logs.js';
import { getChoice, setChoice } from '@/lib/theme.js';
import type { ThemeChoice } from '@/lib/theme.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { collectAllConversationLogs } from '@/lib/migo/collect-chat-logs.js';
import { useConversations } from '@/lib/migo/conversations-provider.js';
import {
  clearChatLogSnapshots,
  isAutoSaveEnabled,
  loadChatLogSnapshots,
  removeChatLogSnapshot,
  setAutoSaveEnabled,
} from '@/lib/storage/chat-log-store.js';

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
 * The Settings tab panel: the grouped settings surface, carrying the device and session lists
 * (§50's own path: Settings → Security Checkup) and the chat-log group.
 */
export function SettingsPanel({
  onOpenCheckup,
  onOpenAccount,
}: {
  /** Opens the Security Checkup panel. Optional so tests can render the panel bare. */
  onOpenCheckup?: () => void;
  /** Opens the My Account panel (identity, email, passphrase, key file). Optional, same rule. */
  onOpenAccount?: () => void;
}): ReactNode {
  const { client } = useMigo();

  const [devices, setDevices] = useState<DeviceSummary[] | null>(null);
  const [removing, setRemoving] = useState<Id | null>(null);
  const [sessions, setSessions] = useState<AccountSession[] | null>(null);
  const [revoking, setRevoking] = useState<Id | null>(null);
  const [signingOut, setSigningOut] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // The saved chat-log snapshots, shared by the Chats & Log group (which lists them) and the
  // Penyimpanan & Data group (whose broom clears them), so one action is reflected in both
  // without either re-reading the store behind the other's back.
  const [snapshots, setSnapshots] = useState<StoredChatLog[] | null>(null);

  const reloadSnapshots = useCallback((): void => {
    void loadChatLogSnapshots()
      .then(setSnapshots)
      .catch(() => {
        // An unreadable store reads as "none" rather than as an error: the list is a convenience
        // over what the auto-save last managed to write, not a promise about it.
        setSnapshots([]);
      });
  }, []);

  useEffect(() => {
    reloadSnapshots();
  }, [reloadSnapshots]);

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

      <ChatsAndLogSection snapshots={snapshots} onSnapshotsChanged={reloadSnapshots} />

      <section className="panel-section" aria-label="Privasi & Keamanan">
        <h2 className="panel-heading">Privasi &amp; Keamanan</h2>
        <p className="hint">
          Every device that can sign in to your account, and every session now signed in.
        </p>

        <h3 className="panel-subheading">Devices</h3>
        {devices === null ? (
          <div className="center-fill">
            <Spinner />
          </div>
        ) : (
          <DeviceList devices={devices} busyId={removing} onRemove={removeDevice} />
        )}

        <h3 className="panel-subheading">Sessions</h3>
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

        <div className="settings-checkup-door">
          <p className="hint">
            Security Checkup: six standing checks — identity, devices, wallets, backup, recovery,
            E2EE — each read from real state.
          </p>
          {onOpenCheckup !== undefined ? (
            <button type="button" className="btn btn-primary" onClick={onOpenCheckup}>
              Open Security Checkup
            </button>
          ) : null}
        </div>
      </section>

      <StorageSection
        snapshots={snapshots}
        onCleared={() => {
          reloadSnapshots();
          setNotice('Saved chat logs cleared. Keys and settings were left untouched.');
        }}
      />

      <AppearanceSection />

      {onOpenAccount !== undefined ? (
        <section className="panel-section" aria-label="Akun">
          <h2 className="panel-heading">Akun</h2>
          <p className="hint">
            Your identity, sign-in email, passphrase, and account key file live in the account
            panel.
          </p>
          <button type="button" className="btn btn-ghost" onClick={onOpenAccount}>
            Open My Account
          </button>
        </section>
      ) : null}

      <AboutSection />
    </div>
  );
}

/**
 * The Chats & Log group: the auto-save toggle, the saved snapshots, and the export-everything
 * walk. Mounted inside the chat shell, so it reads the conversation list from the shared
 * provider the rest of the shell already uses.
 */
function ChatsAndLogSection({
  snapshots,
  onSnapshotsChanged,
}: {
  /** The saved snapshots, or null while the store is being read. */
  snapshots: StoredChatLog[] | null;
  /** Signals that the stored set changed (a delete here, or the broom in the storage group). */
  onSnapshotsChanged: () => void;
}): ReactNode {
  const { client, accountId } = useMigo();
  const { items } = useConversations();

  const [autoSave, setAutoSave] = useState<boolean>(() => isAutoSaveEnabled());
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | null>(null);

  function pickAutoSave(next: boolean): void {
    setAutoSaveEnabled(next);
    setAutoSave(next);
  }

  /**
   * The export-everything walk: re-fetches every conversation's history and replays it through
   * the decrypt path (lib/migo/collect-chat-logs.js), so the file holds what the app can read —
   * not a server-side claim about conversations it can only see as ciphertext.
   */
  function exportAll(): void {
    if (!client || exporting) {
      return;
    }
    setExporting(true);
    setExportError(null);
    void collectAllConversationLogs(client, items, accountId)
      .then((logs) => {
        const file = JSON.stringify({ exportedAt: Date.now(), conversations: logs }, null, 2);
        downloadTextFile(file, logFileName('all chats', 'json'), 'application/json');
      })
      .catch(() => {
        setExportError('The export could not read every conversation; nothing was downloaded.');
      })
      .finally(() => {
        setExporting(false);
      });
  }

  function removeSnapshot(conversationId: Id): void {
    void removeChatLogSnapshot(conversationId).then(onSnapshotsChanged);
  }

  return (
    <section className="panel-section" aria-label="Chats & Log">
      <h2 className="panel-heading">Chats &amp; Log</h2>
      <p className="hint">
        Chat logs are decrypted plaintext. They are saved and downloaded on this device only — the
        server never sees them.
      </p>

      <div className="chip-row" role="group" aria-label="Auto-save chat logs">
        <button
          type="button"
          className={`chip ${!autoSave ? 'chip-active' : ''}`}
          aria-pressed={!autoSave}
          onClick={() => pickAutoSave(false)}
        >
          Off
        </button>
        <button
          type="button"
          className={`chip ${autoSave ? 'chip-active' : ''}`}
          aria-pressed={autoSave}
          onClick={() => pickAutoSave(true)}
        >
          Auto-save
        </button>
      </div>
      <p className="muted">
        When on, each open conversation's transcript is snapshotted on this device every couple of
        minutes, so a closed tab or a crash does not lose the readable record. A snapshot is a
        read-only export — it never re-enters the chat.
      </p>

      <h3 className="panel-subheading">Saved logs</h3>
      {snapshots === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : snapshots.length === 0 ? (
        <p className="muted">No saved logs yet.</p>
      ) : (
        <div className="session-list">
          {snapshots.map((snapshot) => (
            <div key={snapshot.conversationId} className="person-row session-row">
              <div className="person-main">
                <span className="person-name">{snapshot.title}</span>
                <span className="person-sub">
                  saved {formatRelative(snapshot.savedAt)} · {snapshot.messages.length} messages
                </span>
              </div>
              <div className="person-actions">
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={() =>
                    downloadTextFile(
                      formatTranscriptText(snapshot),
                      logFileName(snapshot.title, 'txt'),
                      'text/plain',
                    )
                  }
                >
                  .txt
                </button>
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={() =>
                    downloadTextFile(
                      JSON.stringify(snapshot, null, 2),
                      logFileName(snapshot.title, 'json'),
                      'application/json',
                    )
                  }
                >
                  .json
                </button>
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={() => removeSnapshot(snapshot.conversationId)}
                  aria-label={`Delete saved log for ${snapshot.title}`}
                >
                  ✕
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      <h3 className="panel-subheading">Export</h3>
      {exportError ? <p className="form-error">{exportError}</p> : null}
      <button
        type="button"
        className="btn btn-primary"
        disabled={!client || exporting}
        onClick={exportAll}
      >
        {exporting ? <Spinner /> : 'Export semua chat (.json)'}
      </button>
      <p className="muted">
        One file with every conversation this account holds, re-read from history and decrypted
        here. The per-conversation download lives in the thread's header.
      </p>
    </section>
  );
}

/**
 * The Penyimpanan & Data group: what this device is holding, and the one broom. The estimate is
 * the browser's own accounting (`navigator.storage.estimate`), shown when it exists and honestly
 * absent when it does not; the broom clears the saved chat logs and names what it leaves alone.
 */
function StorageSection({
  snapshots,
  onCleared,
}: {
  /** The saved snapshots, to state how many the broom would remove. */
  snapshots: StoredChatLog[] | null;
  /** Signals that the stored set was cleared. */
  onCleared: () => void;
}): ReactNode {
  const [usage, setUsage] = useState<{ usage: number; quota: number } | null>(null);
  const [clearing, setClearing] = useState(false);

  useEffect(() => {
    // The estimate is informational: a browser without it (or a locked-down embedder) shows the
    // honest absence rather than a zero that would read as "empty".
    try {
      void navigator.storage?.estimate().then((estimate) => {
        if (estimate.usage !== undefined && estimate.quota !== undefined) {
          setUsage({ usage: estimate.usage, quota: estimate.quota });
        }
      });
    } catch {
      // Absent, same as above.
    }
  }, []);

  function clearStored(): void {
    if (clearing) {
      return;
    }
    const confirmed = window.confirm(
      'Delete the saved chat logs on this device? Your keys, settings, and conversations are not touched.',
    );
    if (!confirmed) {
      return;
    }
    setClearing(true);
    void clearChatLogSnapshots()
      .then(onCleared)
      .finally(() => setClearing(false));
  }

  const snapshotCount = snapshots?.length ?? 0;

  return (
    <section className="panel-section" aria-label="Penyimpanan & Data">
      <h2 className="panel-heading">Penyimpanan &amp; Data</h2>
      <p className="hint">
        {usage === null
          ? 'This browser does not report its storage usage.'
          : `This device is holding ${formatBytes(usage.usage)} of ${formatBytes(usage.quota)} the browser offered.`}
      </p>
      <p className="muted">
        {snapshotCount === 0
          ? 'No saved chat logs are on this device.'
          : `${snapshotCount} saved chat log${snapshotCount === 1 ? '' : 's'} ${
              snapshotCount === 1 ? 'is' : 'are'
            } on this device.`}
      </p>
      <button type="button" className="btn btn-ghost" disabled={clearing} onClick={clearStored}>
        {clearing ? <Spinner /> : 'Hapus data tersimpan'}
      </button>
      <p className="muted">
        Removes the saved chat logs only. Your keys, your chosen server, and the theme stay exactly
        as they are.
      </p>
    </section>
  );
}

/**
 * The Tampilan section: the theme, stated as three named choices rather than a toggle.
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
    <section className="panel-section" aria-label="Tampilan">
      <h2 className="panel-heading">Tampilan</h2>
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
