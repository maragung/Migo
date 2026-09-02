'use client';

/**
 * The Owner/CEO's management page for the global admins: who may moderate every
 * public room.
 *
 * The surface is closed by construction, twice. The banner menu only offers the
 * tab after `adminStanding()` says the viewer is the owner, and the server
 * refuses every read and write here for anybody else — so a stale client that
 * kept the tab open after a revocation of the owner designation gets the
 * refusal rendered, not a silent blank. The panel never renders the list
 * before it knows the viewer may see it, because the management page's whole
 * point is that its existence is not public information.
 *
 * The presentational halves are exported as controlled components over plain
 * data, so the rules (the revoke control on every row but never without a
 * confirm, the grant form's empty-input gate, the whoami line that never
 * pretends a refusal is a hidden list) are testable without a live client,
 * exactly like the other panels' extracted pieces.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import type { AdminView, Id } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Spinner } from './spinner.js';

/** One appointed admin, as the owner's list renders it. */
export function AdminRowView({
  admin,
  busy,
  onRevoke,
}: {
  /** The wire row. */
  admin: AdminView;
  /** True while this row's revocation is in flight. */
  busy: boolean;
  /**
   * Requests this admin's revocation. Never called without the surrounding
   * confirm — a revocation takes moderation away from a person, which is not
   * something to do on a stray click.
   */
  onRevoke: (accountId: Id) => void;
}): ReactNode {
  return (
    <div className="person-row session-row">
      <div className="person-main">
        <span className="person-name">
          {admin.username}
          <span className="tag tag-current">Global admin</span>
        </span>
        <span className="person-sub">appointed {formatRelative(admin.grantedAtMs)}</span>
      </div>
      <div className="person-actions">
        <button
          type="button"
          className="btn btn-ghost"
          disabled={busy}
          onClick={() => onRevoke(admin.accountId)}
          aria-label={`Revoke global admin ${admin.username}`}
        >
          Revoke
        </button>
      </div>
    </div>
  );
}

/**
 * The grant form: a username and a button that stays disabled until the name is
 * something. The gate lives in the markup, not only in the handler, so a
 * refactor of the handler cannot ship a form that sends empty requests.
 */
export function GrantAdminFormView({
  username,
  busy,
  onUsername,
  onGrant,
}: {
  /** The current draft. */
  username: string;
  /** True while the grant is in flight. */
  busy: boolean;
  /** Called on every keystroke with the whole draft. */
  onUsername: (value: string) => void;
  /** Requests the appointment (never called with an empty or blank draft). */
  onGrant: (username: string) => void;
}): ReactNode {
  const ready = username.trim() !== '';
  return (
    <form
      className="panel-search"
      onSubmit={(event) => {
        event.preventDefault();
        if (ready && !busy) {
          onGrant(username);
        }
      }}
    >
      <input
        className="input"
        type="text"
        value={username}
        placeholder="username to appoint"
        aria-label="Username to appoint"
        onChange={(event) => onUsername(event.target.value)}
      />
      <button type="submit" className="btn btn-primary" disabled={!ready || busy}>
        Appoint
      </button>
    </form>
  );
}

/** The owner's admin management panel: standing check first, then the CRUD. */
export function AdminsPanel(): ReactNode {
  const { client } = useMigo();

  const [standing, setStanding] = useState<'owner' | 'closed' | null>(null);
  const [admins, setAdmins] = useState<AdminView[] | null>(null);
  const [revoking, setRevoking] = useState<Id | null>(null);
  const [draft, setDraft] = useState('');
  const [granting, setGranting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      const who = await client.adminStanding();
      if (!who.owner) {
        // Not an error to hide behind a spinner: the honest answer is that
        // this deployment's admin surface is not theirs to open.
        setStanding('closed');
        setAdmins(null);
        return;
      }
      setStanding('owner');
      setAdmins(await client.globalAdmins());
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const grant = useCallback(
    (username: string): void => {
      if (!client || granting) {
        return;
      }
      setGranting(true);
      setNotice(null);
      setError(null);
      client
        .grantGlobalAdmin({ username: username.trim() })
        .then((view) => {
          setDraft('');
          setNotice(`${view.username} is now a global admin.`);
          return reload();
        })
        .catch((cause: unknown) => setError(friendlyError(cause)))
        .finally(() => setGranting(false));
    },
    [client, granting, reload],
  );

  const revoke = useCallback(
    (accountId: Id): void => {
      if (!client || revoking !== null || admins === null) {
        return;
      }
      const admin = admins.find((row) => row.accountId === accountId);
      // A revocation takes moderation away from a person, so it is never
      // silent: the owner names the account before the server acts.
      const named = admin ? `“${admin.username}”` : 'this account';
      if (!window.confirm(`Revoke ${named}'s global admin appointment?`)) {
        return;
      }
      setRevoking(accountId);
      setNotice(null);
      setError(null);
      client
        .revokeGlobalAdmin({ account_id: accountId })
        .then(() => {
          setNotice(admin ? `${admin.username} is no longer a global admin.` : 'Revoked.');
          return reload();
        })
        .catch((cause: unknown) => setError(friendlyError(cause)))
        .finally(() => setRevoking(null));
    },
    [admins, client, revoking, reload],
  );

  if (standing === null) {
    return (
      <div className="panel">
        <h1 className="panel-title">Global Admins</h1>
        {error === null ? (
          <div className="center-fill">
            <Spinner />
          </div>
        ) : (
          <p className="form-error">{error}</p>
        )}
      </div>
    );
  }

  if (standing === 'closed') {
    return (
      <div className="panel">
        <h1 className="panel-title">Global Admins</h1>
        <p className="muted">
          This page belongs to the Migo Owner/CEO. Your account cannot open it.
        </p>
      </div>
    );
  }

  return (
    <div className="panel">
      <h1 className="panel-title">Global Admins</h1>
      {error ? <p className="form-error">{error}</p> : null}
      {notice ? <p className="hint">{notice}</p> : null}
      <section className="panel-section" aria-label="Appoint">
        <h2 className="panel-heading">Appoint</h2>
        <p className="hint">
          Global admins moderate every public room. Appointing and revoking is the Owner/CEO&rsquo;s
          alone.
        </p>
        <GrantAdminFormView
          username={draft}
          busy={granting}
          onUsername={setDraft}
          onGrant={grant}
        />
      </section>
      <section className="panel-section" aria-label="Current admins">
        <h2 className="panel-heading">Current admins</h2>
        {admins === null ? (
          <div className="center-fill">
            <Spinner />
          </div>
        ) : admins.length === 0 ? (
          <p className="muted">No global admins yet.</p>
        ) : (
          <div className="session-list">
            {admins.map((admin) => (
              <AdminRowView
                key={admin.accountId}
                admin={admin}
                busy={revoking !== null}
                onRevoke={revoke}
              />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}
