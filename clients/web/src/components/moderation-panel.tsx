'use client';

/**
 * The moderator's dashboard: the queue, one case at a time, the ruling, and the trail.
 *
 * This is the last piece section 49 was missing. The reporting clients can file — user, message,
 * room, bot — and the server has been able to rule since long before any of them could, but the
 * only door onto that side was the REST surface itself, which is a surface for programs. A person
 * with `triage` had no screen: no list of what is waiting, no way to see what a report is about,
 * and no way to close one without a curl command.
 *
 * Three rules shape what this screen may say.
 *
 * **Ids and never usernames.** The moderation surface states ids, and the crate that owns it does
 * not read the account directory — resolving a name to a person is the auth service's business,
 * and a second reader of that directory is a second place a leak can start. So a moderator sees
 * `1G0V…` where they might have expected `alice`, and that is the honest rendering of what the
 * server is willing to tell them.
 *
 * **Nothing about the reported content except the reporter's own words.** Conversations are E2E and
 * a report is a pointer, not a copy: the queue carries the subject's id, the reason code, and the
 * note the reporter chose to write. A dashboard that fetched and displayed the message behind the
 * id would be showing plaintext to a server-side surface, which is exactly what the reporting
 * clients refuse to hand over.
 *
 * **The closing of a case is a decision, so it is never a stray click.** Every ruling button names
 * the ruling it makes, a ruling in flight disables the rest, and a decision that cannot be undone —
 * suspending an account — is a separate surface from the queue's dismiss/warn, because a tired
 * moderator's mis-click should cost a warning and not somebody's account.
 *
 * The pieces are exported as controlled components over plain data so the rules are testable
 * without a live client, like every other panel here.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import type { AuditEntryView, Id, ModerationCase, ModerationStanding } from '@migo/sdk';

import { formatRelative } from '@/lib/format.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Spinner } from './spinner.js';

/**
 * The rulings an operator may choose, in the order they are offered.
 *
 * The codes are the store's `report.resolution` values and they travel as numbers, and the words
 * are this screen's own because the server names only the rulings it has already applied: the
 * vocabulary for a *choice* has nowhere else to live. Escalation is in the list and is not a
 * closing — it hands the case on and leaves it open, which is why it sits last and reads as
 * handing on rather than deciding.
 */
export const RULINGS: { code: number; label: string; closing: boolean }[] = [
  { code: 1, label: 'Warn', closing: true },
  { code: 2, label: 'Remove content', closing: true },
  { code: 0, label: 'No action', closing: true },
  { code: 6, label: 'Invalid report', closing: true },
  { code: 7, label: 'Duplicate', closing: true },
  { code: 5, label: 'Escalate', closing: false },
];

/** The subject kinds the store numbers, for the one place a case's kind is rendered. */
const SUBJECT_WORDS: Record<number, string> = {
  0: 'account',
  1: 'message',
  2: 'room',
  3: 'media',
  4: 'bot',
};

/** A short, human-scannable rendering of an id: enough to match against another screen. */
function shortId(id: string): string {
  return id.length > 10 ? `${id.slice(0, 10)}…` : id;
}

/** One case, as the queue renders it. */
export function CaseRowView({
  entry,
  selected,
  onOpen,
}: {
  /** The case. */
  entry: ModerationCase;
  /** Whether this row is the one open in the detail pane. */
  selected: boolean;
  /** Opens this case. */
  onOpen: (reportId: Id) => void;
}): ReactNode {
  const subject = SUBJECT_WORDS[entry.subjectKind] ?? entry.subjectKindName;
  return (
    <button
      type="button"
      className={`person-row person-row-clickable${selected ? ' person-row-selected' : ''}`}
      aria-pressed={selected}
      onClick={() => onOpen(entry.reportId)}
    >
      <div className="person-main">
        <span className="person-name">
          {entry.reasonName}
          <span className="tag">{subject}</span>
        </span>
        <span className="person-sub">
          {shortId(entry.subjectId)} · filed {formatRelative(entry.createdAtMs)}
          {entry.note ? ' · has a note' : ''}
        </span>
      </div>
      <div className="person-actions">
        <span className="person-sub">{shortId(entry.reportId)}</span>
      </div>
    </button>
  );
}

/**
 * The ruling controls for one case.
 *
 * A ruling in flight disables every button rather than only the one pressed: two moderators
 * clicking two buttons on one case is the ordinary way a case gets two decisions, and the second
 * one is refused by the server anyway — a screen that let it be sent would be a screen that showed
 * an error for something it could have prevented.
 */
export function RulingFormView({
  entry,
  reason,
  busy,
  onReason,
  onRule,
}: {
  /** The case being ruled on. */
  entry: ModerationCase;
  /** The operator's own words, stored on the audit row. */
  reason: string;
  /** True while a ruling is in flight. */
  busy: boolean;
  /** Called on every keystroke with the whole draft. */
  onReason: (value: string) => void;
  /** Requests one ruling by its code. */
  onRule: (code: number) => void;
}): ReactNode {
  return (
    <div className="panel-section" aria-label="Rule on this case">
      <p className="hint">
        A ruling closes this case and is recorded with your account on the audit trail. The reporter
        is told the outcome and nothing about the account it was about.
      </p>
      <input
        className="input"
        type="text"
        value={reason}
        placeholder="why (optional, recorded on the audit trail)"
        aria-label="Reason for the ruling"
        onChange={(event) => onReason(event.target.value)}
      />
      <div className="badge-row">
        {RULINGS.map((ruling) => (
          <button
            key={ruling.code}
            type="button"
            className={ruling.closing ? 'btn btn-primary' : 'btn btn-ghost'}
            disabled={busy}
            onClick={() => onRule(ruling.code)}
            aria-label={`${ruling.label} on the report about ${entry.subjectKindName} ${entry.subjectId}`}
          >
            {ruling.label}
          </button>
        ))}
      </div>
    </div>
  );
}

/** The audit trail for whatever target the caller asked about, newest first. */
export function AuditTrailView({ entries }: { entries: AuditEntryView[] }): ReactNode {
  if (entries.length === 0) {
    return <p className="muted">Nothing has been recorded against this case yet.</p>;
  }
  return (
    <div className="session-list">
      {entries.map((entry) => (
        <div className="person-row session-row" key={entry.auditId}>
          <div className="person-main">
            <span className="person-name">
              {entry.action}
              <span className="tag">{entry.actorKindName}</span>
            </span>
            <span className="person-sub">
              {entry.summary} · {formatRelative(entry.createdAtMs)}
              {entry.actorId ? ` · ${shortId(entry.actorId)}` : ''}
            </span>
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * One case's facts, as the store states them.
 *
 * The note is the reporter's own words and is the only free text on this screen: everything else is
 * an id, a code, or a time, so a moderator reading a case is reading the report and not a rendering
 * of the conversation it points at.
 */
function CaseFactsView({ entry }: { entry: ModerationCase }): ReactNode {
  return (
    <dl className="case-facts">
      <dt>Case</dt>
      <dd>{entry.reportId}</dd>
      <dt>Reported</dt>
      <dd>
        {SUBJECT_WORDS[entry.subjectKind] ?? entry.subjectKindName} {entry.subjectId}
      </dd>
      <dt>Reason</dt>
      <dd>{entry.reasonName}</dd>
      <dt>Filed by</dt>
      <dd>{entry.reporterId}</dd>
      {entry.roomId ? (
        <>
          <dt>Room</dt>
          <dd>{entry.roomId}</dd>
        </>
      ) : null}
      <dt>State</dt>
      <dd>
        {entry.open
          ? 'open'
          : `${entry.resolutionName ?? `resolution ${entry.resolution ?? '?'}`}${
              entry.resolvedAtMs === undefined ? '' : ` · ${formatRelative(entry.resolvedAtMs)}`
            }`}
      </dd>
      {entry.note ? (
        <>
          <dt>Their words</dt>
          <dd className="case-note">{entry.note}</dd>
        </>
      ) : null}
    </dl>
  );
}

/** The moderator's dashboard. */
export function ModerationPanel(): ReactNode {
  const { client } = useMigo();

  const [standing, setStanding] = useState<ModerationStanding | null>(null);
  const [closed, setClosed] = useState(false);
  const [queue, setQueue] = useState<ModerationCase[] | null>(null);
  const [selected, setSelected] = useState<Id | null>(null);
  const [trail, setTrail] = useState<AuditEntryView[] | null>(null);
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const loadQueue = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    const who = await client.moderationStanding();
    setStanding(who);
    if (!who.staff) {
      // Not an error to hide behind a spinner: the honest answer is that this account holds no
      // powers, so there is no operator surface to render.
      setClosed(true);
      return;
    }
    setQueue(await client.moderationQueue());
  }, [client]);

  const reload = useCallback(async (): Promise<void> => {
    if (!client) {
      return;
    }
    try {
      await loadQueue();
      setError(null);
    } catch (cause) {
      setError(friendlyError(cause));
    }
  }, [client, loadQueue]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const openCase = useCallback(
    (reportId: Id): void => {
      if (!client) {
        return;
      }
      setSelected(reportId);
      setTrail(null);
      setNotice(null);
      setError(null);
      // The trail is a separate power from the queue, so a triager without `audit` gets the case
      // and no trail rather than a refusal rendered over a screen that otherwise works.
      if (standing !== null && standing.powers.includes('audit')) {
        client
          .moderationAudit({ target_kind: 'report', target_id: reportId })
          .then(setTrail)
          .catch((cause: unknown) => setError(friendlyError(cause)));
      }
    },
    [client, standing],
  );

  const rule = useCallback(
    (code: number): void => {
      if (!client || selected === null || busy) {
        return;
      }
      const entry = queue?.find((row) => row.reportId === selected) ?? null;
      const ruling = RULINGS.find((candidate) => candidate.code === code);
      const named = entry ? `${entry.reasonName} about ${shortId(entry.subjectId)}` : 'this report';
      // Never silent: the moderator names what they are deciding before the server records it.
      if (!window.confirm(`${ruling?.label ?? 'Rule'} on ${named}?`)) {
        return;
      }
      setBusy(true);
      setNotice(null);
      setError(null);
      client
        .resolveModerationCase({
          report_id: selected,
          resolution: code,
          ...(reason.trim() !== '' ? { reason: reason.trim() } : {}),
        })
        .then((updated) => {
          setReason('');
          setNotice(
            updated.open
              ? 'Escalated — the case stays in the queue for whoever it was handed to.'
              : 'Recorded. The reporter has been told the outcome.',
          );
          return reload();
        })
        .catch((cause: unknown) => setError(friendlyError(cause)))
        .finally(() => setBusy(false));
    },
    [busy, client, queue, reason, reload, selected],
  );

  if (closed) {
    return (
      <div className="panel">
        <h1 className="panel-title">Moderation</h1>
        <p className="muted">
          This account holds no moderation powers on this node, so there is nothing here to open.
        </p>
      </div>
    );
  }

  if (standing === null || queue === null) {
    return (
      <div className="panel">
        <h1 className="panel-title">Moderation</h1>
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

  const entry = queue.find((row) => row.reportId === selected) ?? null;

  return (
    <div className="panel">
      <h1 className="panel-title">Moderation</h1>
      <p className="hint">
        Powers: {standing.powers.length === 0 ? 'none' : standing.powers.join(', ')}. Reports are
        listed by id: the moderation surface does not read the account directory, so a name is not
        something this screen can show.
      </p>
      {error ? <p className="form-error">{error}</p> : null}
      {notice ? <p className="hint">{notice}</p> : null}
      <section className="panel-section" aria-label="Open reports">
        <h2 className="panel-heading">Waiting</h2>
        {queue.length === 0 ? (
          <p className="muted">Nothing is waiting. The queue is empty.</p>
        ) : (
          <div className="session-list">
            {queue.map((row) => (
              <CaseRowView
                key={row.reportId}
                entry={row}
                selected={row.reportId === selected}
                onOpen={openCase}
              />
            ))}
          </div>
        )}
      </section>
      {entry ? (
        <section className="panel-section" aria-label="Case">
          <h2 className="panel-heading">Case {shortId(entry.reportId)}</h2>
          <CaseFactsView entry={entry} />
          {entry.open ? (
            <RulingFormView
              entry={entry}
              reason={reason}
              busy={busy}
              onReason={setReason}
              onRule={rule}
            />
          ) : (
            <p className="muted">This case is closed. A closed case is not ruled on twice.</p>
          )}
        </section>
      ) : null}
      {entry && standing.powers.includes('audit') ? (
        <section className="panel-section" aria-label="Audit trail">
          <h2 className="panel-heading">Trail</h2>
          {trail === null ? (
            <div className="center-fill">
              <Spinner />
            </div>
          ) : (
            <AuditTrailView entries={trail} />
          )}
        </section>
      ) : null}
    </div>
  );
}
