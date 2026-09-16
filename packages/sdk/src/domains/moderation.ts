/**
 * The moderation domain: pointing the node's warden at something.
 *
 * Section 49 of the brief asks for four kinds of report — a user, a message, a room, a bot — and
 * that is exactly the vocabulary {@link ReportSubject} carries. Filing one is the only thing this
 * domain does, and it is deliberately the only thing a *client* can do: reading the queue, ruling
 * on a case, and applying a takedown are staff powers, and the surface for them is the node's
 * operator API, not this one. A client that could read the queue would be a client that could read
 * who reported whom.
 *
 * # A report is a pointer, never a copy
 *
 * The wire carries a subject kind and a subject id, plus a reason code and an optional note the
 * reporter wrote. It carries no message content and no attachment, and that is not a shortcut: the
 * conversations this system carries are end-to-end encrypted, so a report that quoted the offending
 * text would either be unreadable to the moderator or readable only by shipping the server a
 * plaintext it is not supposed to have. The moderator follows the pointer with their own eyes.
 *
 * The corollary is the note: {@link ReportOptions.note} is the *reporter's own words*, typed by
 * them, and it is the only field in this whole path a human wrote. It is stored on the report row
 * and never on the audit entry, which names the reason code instead.
 *
 * # Idempotent, and free of charge
 *
 * A second report from the same reporter about the same still-open subject does not create a second
 * row and does not fail — the server recognises it and answers success, because the usual cause is
 * a client whose first answer was lost and telling it the report failed would be a lie about a
 * report that is in fact sitting in the queue (brief section 153). Reporting yourself is refused as
 * a client bug rather than queued for a human to read.
 *
 * Filing is priced (the registry charges REPORT_CREATE 20), so a client that offers a report button
 * offers it once per gesture rather than retrying a rejected call in a loop.
 *
 * # Why the reply is a bare acknowledgement
 *
 * `REPORT_CREATE` answers with {@link Acknowledged}, not with the report's id, so this domain
 * resolves with nothing rather than with a handle. The server knows more than it says here — it
 * knows whether the filing was a duplicate and which row it landed on — and the decision to keep
 * the reply bare is deliberate: a reporter has no use for a case id (they cannot read the case),
 * and echoing one back would invite a client to present it as a receipt the reporter could chase.
 * The one thing a client legitimately needs — that the report arrived — is what the ack carries.
 */

import type { Id } from '@migo/wire';
import { OP, encodeReportFile, decodeAcknowledged, decodeModerationEvent } from '@migo/protocol';
import type { ModerationEvent, ReportFile } from '@migo/protocol';

import { ListenerSet } from './listeners.js';
import type { Listener } from './listeners.js';
import type { EventErrorHandler, Rpc } from './rpc.js';

/**
 * What is being reported, as the wire numbers it.
 *
 * The values are the `subject_kind` field of `REPORT_CREATE` and are load-bearing: they are what
 * the node maps to its own storage vocabulary, so they are never renumbered or reused. `Message`
 * is a message id and nothing more — the conversation it sits in is not on the wire (see
 * {@link ModerationDomain.reportMessage}), and a report row stores exactly one id.
 */
export enum ReportSubject {
  /** A whole account, by account id. */
  User = 0,
  /** One message, by message id. */
  Message = 1,
  /** A room, by room id. */
  Room = 2,
  /** A bot, by `bot.bot_id` rather than by the account it signs in as. */
  Bot = 3,
}

/**
 * Why something is being reported.
 *
 * The codes mirror the node's own reason vocabulary and are stored as given, so they are never
 * renumbered. The four that exist for a legal reason rather than a product one —
 * {@link ReportReason.ChildSafety} above all, kept separate from
 * {@link ReportReason.SexualContent} because the obligations attached to it are not the same and an
 * operator must be able to filter the queue for exactly it — are the reason this is a code and not
 * a free-text field.
 *
 * {@link ReportReason.SelfHarm} is routed like any other report and prioritised like none of them:
 * this domain carries the code, and what a deployment does with it afterwards is a staffing
 * question no amount of client code answers.
 */
export enum ReportReason {
  /** Unsolicited bulk content. */
  Spam = 0,
  /** Volume rather than content: the same thing, very fast. */
  Flood = 1,
  /** An attempt to obtain money or credentials by deception. */
  Scam = 2,
  /** A link to malware, phishing, or a credential harvester. */
  MaliciousLink = 3,
  /** Harassment, threats, or targeted abuse of a person. */
  Harassment = 4,
  /** Hateful content aimed at a group. */
  HateSpeech = 5,
  /** Sexual content where it does not belong. */
  SexualContent = 6,
  /** Graphic violence. */
  Violence = 7,
  /** Self-harm or suicide content. */
  SelfHarm = 8,
  /** Somebody pretending to be somebody else. */
  Impersonation = 9,
  /** Child sexual abuse material. Kept its own code; see the enum's own note. */
  ChildSafety = 10,
  /** A bot misbehaving: a broken integration rather than an abusive person. */
  BotAbuse = 11,
  /** None of the above. */
  Other = 12,
}

/**
 * The longest note the node accepts, in characters.
 *
 * Mirrors the warden's own `MAX_NOTE_LEN`. {@link ModerationDomain.report} checks it here so an
 * over-long note fails without spending the frame or the report's cost on a call that can only be
 * refused.
 */
export const REPORT_NOTE_MAX_LEN = 500;

/** Optional parameters for {@link ModerationDomain.report}. */
export interface ReportOptions {
  /**
   * The reporter's own words about what happened, at most {@link REPORT_NOTE_MAX_LEN} characters.
   *
   * The only human-written field in the path. Left off, the report is the reason code alone.
   */
  note?: string;
}

/** What a report points at: a kind from {@link ReportSubject} and the id of that thing. */
export interface ReportTarget {
  /** Which of the four things is being reported. */
  kind: ReportSubject;
  /** The id of that thing, in the vocabulary {@link ReportSubject} documents. */
  id: Id;
}

/**
 * File reports, and receive the node's word when one is ruled on.
 *
 * One instance per client. {@link report} works on its own; {@link onModerationEvent} only fires
 * once {@link start} has been called.
 */
export class ModerationDomain {
  readonly #rpc: Rpc;
  readonly #listeners: ListenerSet<ModerationEvent>;
  #unsubscribe: (() => void) | null = null;

  constructor(rpc: Rpc, onEventError?: EventErrorHandler) {
    this.#rpc = rpc;
    this.#listeners = new ListenerSet(OP.MODERATION_EVENT, onEventError);
  }

  /** Begins delivering inbound moderation events to registered handlers. Idempotent. */
  start(): void {
    if (this.#unsubscribe !== null) {
      return;
    }
    this.#unsubscribe = this.#rpc.on(OP.MODERATION_EVENT, decodeModerationEvent, (event) =>
      this.#listeners.deliver(event),
    );
  }

  /** Stops delivering moderation events. Registered handlers are kept for a later {@link start}. */
  stop(): void {
    this.#unsubscribe?.();
    this.#unsubscribe = null;
  }

  /**
   * Registers a handler for moderation events — the node's word that a case was decided.
   *
   * Returns an unsubscribe function. A client renders this as "your report was reviewed"; the event
   * names the case, the ruling code, and its state, and carries nothing about the subject, because
   * what happened to somebody else's account is not the reporter's to read.
   */
  onModerationEvent(handler: Listener<ModerationEvent>): () => void {
    return this.#listeners.add(handler);
  }

  /**
   * Files a report about `target`, with `reason` explaining why.
   *
   * Resolves when the node has taken the report — see this module's note on why the answer is an
   * acknowledgement and not a case id. Rejects with a {@link RemoteError} when the node refuses:
   * an unknown subject kind, a note over {@link REPORT_NOTE_MAX_LEN}, the caller's own account as
   * the subject, or a rate limit the caller has earned.
   *
   * The report is scoped to the caller's account, so the same account filing twice about the same
   * still-open subject succeeds both times and leaves one row.
   */
  async report(
    target: ReportTarget,
    reason: ReportReason,
    options: ReportOptions = {},
  ): Promise<void> {
    const note = options.note;
    if (note !== undefined && note.length > REPORT_NOTE_MAX_LEN) {
      throw new RangeError(
        `a report note is at most ${REPORT_NOTE_MAX_LEN} characters, got ${note.length}`,
      );
    }
    const request: ReportFile = {
      subjectKind: target.kind,
      subjectId: target.id,
      reason,
    };
    if (note !== undefined) {
      request.note = note;
    }
    await this.#rpc.call(OP.REPORT_CREATE, encodeReportFile, decodeAcknowledged, request);
  }

  /**
   * Files a report about one message, identified by its id alone.
   *
   * The conversation is deliberately not a parameter. A report row holds a single id and the wire
   * carries a single id, so naming the conversation here would promise a precision the protocol
   * does not have — and a caller who passed the wrong one would be filing about the right message
   * under a key nothing reads. A moderator reaches the message through the report's own subject,
   * not through a conversation id the reporter supplied.
   */
  async reportMessage(
    messageId: Id,
    reason: ReportReason,
    options: ReportOptions = {},
  ): Promise<void> {
    await this.report({ kind: ReportSubject.Message, id: messageId }, reason, options);
  }

  /** Files a report about one account. The node refuses the caller's own account. */
  async reportUser(
    accountId: Id,
    reason: ReportReason,
    options: ReportOptions = {},
  ): Promise<void> {
    await this.report({ kind: ReportSubject.User, id: accountId }, reason, options);
  }

  /** Files a report about one room. */
  async reportRoom(roomId: Id, reason: ReportReason, options: ReportOptions = {}): Promise<void> {
    await this.report({ kind: ReportSubject.Room, id: roomId }, reason, options);
  }

  /**
   * Files a report about one bot, by `bot.bot_id`.
   *
   * A bot is reported as a bot and not as its owner's account, because the two are different
   * problems with different remedies: a moderator reading the queue needs to tell "this
   * integration is broken" from "this person is abusive" before deciding anything.
   */
  async reportBot(botId: Id, reason: ReportReason, options: ReportOptions = {}): Promise<void> {
    await this.report({ kind: ReportSubject.Bot, id: botId }, reason, options);
  }
}
