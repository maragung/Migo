'use client';

import { useCallback, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';

import { CallKind, CallDirection, CallOutcome, CallMediaKind } from '@migo/sdk';
import type { CallHistoryEntry, Id } from '@migo/sdk';

import { formatDayLabel, formatClock } from '@/lib/format.js';
import { callDirectionOf, callKindOf, callOutcomeOf } from '@/lib/migo/call-signal.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';
import { useProfiles } from '@/lib/migo/use-profiles.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import { Spinner } from './spinner.js';

/**
 * The history page the panel asks for, and the ceiling the server clamps it to.
 *
 * The server owns the real ceiling; this is the size this screen renders well, and a page smaller
 * than the window's height is what the "Load older calls" button is for.
 */
const PAGE_SIZE = 50;

/**
 * The Call history window: what this account's calls came to.
 *
 * The one call surface that reads the past. Every other screen in the app is about a call that is
 * happening — the ring, the roster, the media — and the server's listing is exactly that: a filter
 * on calls that are still alive, so an ended call falls out of it the moment it ends. This panel
 * asks the other question, and the server answers it from the row it wrote when the call died.
 *
 * Outcome is read from the row and never reconstructed here: the server derives it from its own
 * record of whether the callee picked up, so a call this account missed reads as missed no matter
 * what either party said afterwards. Direction is derived the same way and from this account's
 * side, so the arrow is drawn from a fact rather than from a comparison.
 *
 * A call this account can place again offers the door back: a direct row carries the peer and a
 * group row carries its conversation, and both are handed to the host shell, which owns what
 * "calling someone" means. The panel places no call itself — it has no media, and a screen that
 * could start one from a row would be a second place a call begins.
 */
export function CallHistoryPanel({
  conversationId,
  onCallPeer,
  onOpenConversation,
}: {
  /** Scopes the history to one conversation, which is the question a conversation's own screen asks. */
  conversationId?: Id;
  /** Starts a call back to a direct row's peer; absent leaves the row a record to read. */
  onCallPeer?: (peerId: Id, video: boolean) => void;
  /** Opens the conversation a row belongs to; absent leaves the row a record to read. */
  onOpenConversation?: (conversationId: Id) => void;
}): ReactNode {
  const { client } = useMigo();

  // Pages are held as one list, oldest page appended behind the newest, because that is the order
  // the server answers in and the order the screen draws: paging back is asking for what happened
  // before what is already on screen.
  const [rows, setRows] = useState<CallHistoryEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Whether the last page came back full. A short page is the end of the history, and the button
  // that would ask for more is not offered past it.
  const [more, setMore] = useState(false);
  const [busy, setBusy] = useState(false);

  const scope = conversationId;

  const load = useCallback(
    async (before: number | undefined): Promise<void> => {
      if (!client) {
        return;
      }
      setBusy(true);
      try {
        const query = {
          ...(scope !== undefined ? { conversationId: scope } : {}),
          ...(before !== undefined ? { before } : {}),
          limit: PAGE_SIZE,
        };
        const page = await client.calls.callHistory(query);
        setRows((prev) => (before === undefined ? page : [...(prev ?? []), ...page]));
        setMore(page.length === PAGE_SIZE);
        setError(null);
      } catch (cause) {
        setError(friendlyError(cause));
      } finally {
        setBusy(false);
      }
    },
    [client, scope],
  );

  // A change of scope is a different history, so the list starts over rather than being appended
  // to: a conversation's screen and the account's own would otherwise share one list.
  useEffect(() => {
    setRows(null);
    void load(undefined);
  }, [load]);

  /** Asks for the page behind the oldest row held; that row's own `endedAt` is the cursor. */
  async function loadOlder(): Promise<void> {
    const oldest = rows?.[rows.length - 1];
    if (oldest === undefined || busy) {
      return;
    }
    await load(oldest.endedAt);
  }

  const peerIds = useMemo(() => [...new Set((rows ?? []).map((row) => row.peerId))], [rows]);
  const profiles = useProfiles(peerIds);

  return (
    <div className="panel">
      <header className="panel-head">
        <h1 className="panel-title">Calls</h1>
      </header>

      {error ? <p className="form-error">{error}</p> : null}

      {rows === null ? (
        <div className="center-fill">
          <Spinner />
        </div>
      ) : rows.length === 0 ? (
        <div className="center-fill">
          <div>
            <div className="emoji">
              <Icon name="phone" size={24} />
            </div>
            No calls yet.
          </div>
        </div>
      ) : (
        <>
          <ul className="notification-list">
            {rows.map((row) => (
              <CallHistoryRow
                key={row.callId}
                row={row}
                peerName={profiles.get(row.peerId)?.displayName ?? null}
                onCallPeer={onCallPeer}
                onOpenConversation={onOpenConversation}
              />
            ))}
          </ul>
          {more ? (
            <div className="panel-actions">
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() => void loadOlder()}
              >
                {busy ? <Spinner /> : 'Load older calls'}
              </button>
            </div>
          ) : null}
        </>
      )}
    </div>
  );
}

/**
 * The sentence a row reads as, from its outcome and its direction.
 *
 * Direction is what makes the sentence honest: an outgoing call that rang out was not "missed", it
 * was unanswered by the other party, and a screen that printed the same word for both would be
 * telling the user they missed a call they placed.
 */
function outcomeSentence(outcome: CallOutcome | undefined, outgoing: boolean): string {
  switch (outcome) {
    case CallOutcome.Answered:
      return outgoing ? 'Outgoing' : 'Incoming';
    case CallOutcome.Missed:
      return outgoing ? 'No answer' : 'Missed';
    case CallOutcome.Declined:
      return outgoing ? 'Declined' : 'Declined';
    case CallOutcome.Busy:
      return outgoing ? 'Busy' : 'Missed on another call';
    case CallOutcome.Cancelled:
      return 'Cancelled';
    case CallOutcome.Failed:
      return 'Failed';
    default:
      // A number this build does not know is a server that has moved ahead of it; the row is
      // still a call, so it is drawn as one rather than dropped.
      return 'Call';
  }
}

/** The icon a row carries: the direction the call went, which is the one fact the glyph holds.
 *
 * The outcome is the sentence beside it rather than a second glyph: an arrow out that rang out and
 * an arrow out that was answered are the same direction, and a screen that drew them differently
 * would be asking the icon to say two things at once. */
function directionIcon(row: CallHistoryEntry): 'arrow-up' | 'arrow-down' {
  return callDirectionOf(row.direction) === CallDirection.Outgoing ? 'arrow-up' : 'arrow-down';
}

/** Milliseconds an answered call lasted, or null when it never connected. */
function durationMs(row: CallHistoryEntry): number | null {
  if (row.answeredAt === undefined) {
    return null;
  }
  return Math.max(0, row.endedAt - row.answeredAt);
}

/** A duration as a person reads it: seconds under a minute, then minutes, then hours. */
function formatDuration(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) {
    return `${seconds}s`;
  }
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes}m ${seconds % 60}s`;
  }
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/**
 * One call-history row: who, which way, how it ended, and — when it connected — how long it lasted.
 *
 * Exported presentational over plain props, so what a row offers is testable without a live client:
 * a direct row offers a call back when the host gave it a door, a group row offers the conversation
 * it happened in, and a row of either kind with no door stays inert.
 */
export function CallHistoryRow({
  row,
  peerName,
  onCallPeer,
  onOpenConversation,
}: {
  row: CallHistoryEntry;
  peerName: string | null;
  onCallPeer?: (peerId: Id, video: boolean) => void;
  onOpenConversation?: (conversationId: Id) => void;
}): ReactNode {
  // Narrowed at the boundary, once each: the wire sends these as bare numbers, and the rest of
  // this component reasons about the enums rather than about integers that happen to line up.
  const direction = callDirectionOf(row.direction);
  const outcome = callOutcomeOf(row.outcome);
  const outgoing = direction === CallDirection.Outgoing;
  const answered = outcome === CallOutcome.Answered;
  const duration = durationMs(row);
  const group = callKindOf(row.kind) === CallKind.Group;
  const video = row.mediaKind === CallMediaKind.Video;
  const name =
    peerName !== null && peerName.length > 0 ? peerName : group ? 'Group call' : 'Unknown';

  // What the row's button does, if anything: a direct call can be placed again, and a group call
  // is a conversation the user can go back to. A row whose host offered no door renders none.
  const action =
    group && onOpenConversation !== undefined ? (
      <button type="button" className="btn" onClick={() => onOpenConversation(row.conversationId)}>
        Open
      </button>
    ) : !group && onCallPeer !== undefined ? (
      <button
        type="button"
        className="btn"
        title={video ? 'Call back with video' : 'Call back'}
        onClick={() => onCallPeer(row.peerId, video)}
      >
        <Icon name={video ? 'video' : 'phone'} size={14} />
      </button>
    ) : undefined;

  return (
    <li className={`notification-row call-history-row${answered ? '' : ' call-history-missed'}`}>
      <Avatar id={row.peerId} name={name} />
      <div className="call-history-body">
        <div className="call-history-name">{name}</div>
        <div className="call-history-meta">
          <Icon name={directionIcon(row)} size={12} />
          <span>{outcomeSentence(outcome, outgoing)}</span>
          <span className="call-history-time">
            {formatDayLabel(row.endedAt)} {formatClock(row.endedAt)}
          </span>
          {duration !== null ? (
            <span className="call-history-duration">{formatDuration(duration)}</span>
          ) : null}
          {group && row.participantCount !== undefined ? (
            <span className="call-history-count">{row.participantCount} people</span>
          ) : null}
        </div>
      </div>
      {action}
    </li>
  );
}
