/**
 * The offline outbox: sends that left the composer but not yet the device.
 *
 * Section 158's offline-first contract has a client half: a message composed while the link is
 * down is *queued*, not failed, and it leaves the device once sync has caught up — with the
 * order inside each conversation preserved, because a reply that overtakes the message it
 * answers is a conversation neither side can read. This queue is that half. An entry is minted
 * exactly once (its {@link OutboxEntry.messageId} is the idempotency key the server dedups on,
 * so a retry after a lost reply — where the send landed but the acknowledgement did not — is
 * answered `duplicate` and produces no second row), and its promise settles only when the
 * server has acknowledged it or the attempt budget is spent: the composer's optimistic echo
 * can key off the same promise it would have keyed off a direct send.
 *
 * # Which failures come back, and how far
 *
 * The retry rules are section 151's, applied per entry:
 *
 * * The link failing — a request that timed out or rode a socket that died mid-flight —
 *   counts as an attempt and comes back with backoff, because the frame may have landed and
 *   only the idempotency key makes the retry safe.
 * * A server refusal in a retryable class (RateLimit, Server, Federation) comes back with
 *   backoff — the server's `retry_after_ms` when it named one, which is honoured as given:
 *   ignoring it would pour fuel on the overload that caused it.
 * * A refusal in any other class (Protocol, Auth, Permission, Validation, State) fails the
 *   entry outright and rejects its promise: retrying a refusal the server meant can only
 *   produce the same refusal, later.
 * * After {@link OutboxOptions.maxAttempts} attempts the entry is marked failed, its promise
 *   rejects with the last error, and the composer shows it — an item retried forever is a
 *   message the user believes was sent and never was, which is the one outcome worse than a
 *   visible failure.
 *
 * A transport that is simply not ready parks the drain without counting an attempt: nothing
 * left, so there is nothing to retry — the drain restarts when the client reports readiness
 * again (a reconnect that resumed, or a fresh session's resubscription finishing). And while
 * the {@link SyncGate} is closed — the page hidden — the drain parks between entries rather
 * than burning battery, per section 158's "stopped neatly, not left running".
 *
 * Typing never enters this queue (section 159: typing is not queued while offline) — a typing
 * indicator that arrives after the typing stopped is noise with a delivery receipt. Receipts
 * do not either: they are watermarks, and the next one after reconnect covers what a queued
 * one would have said.
 */

import type { Id } from '@migo/wire';
import { errorClass, isRetryable } from '@migo/protocol';
import type { MessageAccepted } from '@migo/protocol';

import type { MessageContent } from './content.js';
import { RemoteError, SdkError, TimeoutError, TransportError } from './errors.js';
import type { SendOptions } from './domains/messaging.js';
import { newId } from './ids.js';
import type { SyncGate } from './sync-gate.js';

/** Where an entry stands, for the composer's "sending…" and failure surfaces. */
export type OutboxState = 'queued' | 'sending' | 'delivered' | 'failed';

/** A snapshot of one entry, as a UI reads it. Never carries the content itself. */
export interface OutboxEntry {
  /** The idempotency key: minted at enqueue, reused on every retry of this entry. */
  messageId: Id;
  /** The conversation the message is bound for. */
  conversationId: Id;
  /** Where the entry stands. */
  state: OutboxState;
  /** Send attempts that left the device (a parked drain does not count). */
  attempts: number;
}

/** How the outbox is tuned; every field has a default a stock client can live with. */
export interface OutboxOptions {
  /** Attempts that leave the device before an entry is failed; default 5. */
  maxAttempts?: number;
  /** Base for the exponential retry backoff, in milliseconds; default 500. */
  backoffBaseMs?: number;
  /** The ceiling for one retry backoff, in milliseconds; default 30000. */
  backoffCapMs?: number;
  /** Queue capacity; an enqueue past it rejects, because memory is not an outbox. Default 128. */
  maxEntries?: number;
}

/** Performs one send attempt: the messaging domain's own send, with the entry's id reused. */
export type OutboxSender = (
  conversationId: Id,
  content: MessageContent,
  options: SendOptions,
) => Promise<MessageAccepted>;

/** Whether the link is ready to carry a request right now. */
export type OutboxReadiness = () => boolean;

/** A handler notified whenever an entry's state changes. */
export type OutboxListener = (entry: OutboxEntry) => void;

const DEFAULT_MAX_ATTEMPTS = 5;
const DEFAULT_BACKOFF_BASE_MS = 500;
const DEFAULT_BACKOFF_CAP_MS = 30_000;
const DEFAULT_MAX_ENTRIES = 128;

/** One queued send: everything a retry needs, plus the promise the composer holds. */
interface Queued {
  readonly messageId: Id;
  readonly conversationId: Id;
  readonly content: MessageContent;
  readonly options: SendOptions;
  state: OutboxState;
  attempts: number;
  lastError: SdkError | undefined;
  resolve: (accepted: MessageAccepted) => void;
  reject: (error: SdkError) => void;
}

/**
 * The queue of undelivered sends.
 *
 * One instance per client, held across sessions: a disconnect does not fail the queue (a
 * {@link MigoClient.resume} re-establishes the transport and the entries drain against it),
 * because the moment a user most wants their drafted messages to leave is the moment they
 * come back online. The queue is FIFO, which preserves per-conversation order as a
 * consequence — section 158 asks for that order and nothing about interleaving distinct
 * conversations, and a single lane is also the cheapest way to keep one conversation's
 * replies behind its message.
 */
export class Outbox {
  readonly #send: OutboxSender;
  readonly #isReady: OutboxReadiness;
  readonly #gate: SyncGate;
  readonly #maxAttempts: number;
  readonly #backoffBaseMs: number;
  readonly #backoffCapMs: number;
  readonly #maxEntries: number;
  readonly #listeners = new Set<OutboxListener>();

  readonly #queue: Queued[] = [];
  #draining = false;

  constructor(
    send: OutboxSender,
    isReady: OutboxReadiness,
    gate: SyncGate,
    options: OutboxOptions = {},
  ) {
    this.#send = send;
    this.#isReady = isReady;
    this.#gate = gate;
    this.#maxAttempts = options.maxAttempts ?? DEFAULT_MAX_ATTEMPTS;
    this.#backoffBaseMs = options.backoffBaseMs ?? DEFAULT_BACKOFF_BASE_MS;
    this.#backoffCapMs = options.backoffCapMs ?? DEFAULT_BACKOFF_CAP_MS;
    this.#maxEntries = options.maxEntries ?? DEFAULT_MAX_ENTRIES;
  }

  /** Entries waiting to leave, in queue order. A snapshot, not a live view. */
  entries(): OutboxEntry[] {
    return this.#queue.map((entry) => this.#snapshot(entry));
  }

  /** How many entries are undelivered. */
  get size(): number {
    return this.#queue.length;
  }

  /** Registers a handler for entry state changes. Returns an unsubscribe function. */
  onEntryChange(listener: OutboxListener): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  /**
   * Queues a send, or performs it now if the link is ready and the gate open.
   *
   * Resolves with the server's acknowledgement — immediately for an online send, after the
   * retry cycle for an offline one — and rejects when the entry fails: a non-retryable
   * refusal, or the attempt budget spent. The caller's optimistic echo belongs on the resolve
   * path exactly as it would on a direct {@link MessagingDomain.send}.
   */
  send(
    conversationId: Id,
    content: MessageContent,
    options: SendOptions = {},
  ): Promise<MessageAccepted> {
    if (this.#queue.length >= this.#maxEntries) {
      return Promise.reject(
        new SdkError(`migo: outbox is full (${this.#maxEntries} entries); send rejected`),
      );
    }
    // The idempotency key is minted here, once, and reused on every attempt: the server's
    // send dedup is keyed on it, so a retry that lands after a lost reply is answered
    // `duplicate` rather than stored twice. A caller-supplied id is honoured the same way
    // the direct send honours it.
    const withId: SendOptions = { ...options, messageId: options.messageId ?? newId() };
    return new Promise<MessageAccepted>((resolve, reject) => {
      this.#queue.push({
        messageId: withId.messageId ?? newId(),
        conversationId,
        content,
        options: withId,
        state: 'queued',
        attempts: 0,
        lastError: undefined,
        resolve,
        reject,
      });
      this.#notifyTail();
      this.drain();
    });
  }

  /**
   * Starts (or restarts) the drain if it is not already running.
   *
   * Idempotent and safe to call from anywhere — every readiness transition, every visibility
   * change, every enqueue simply kicks it; the loop itself decides whether there is anything
   * to do. When the link is not ready it returns immediately and waits for the next kick,
   * which the client issues when readiness returns.
   */
  drain(): void {
    if (this.#draining) {
      return;
    }
    this.#draining = true;
    void this.#drainLoop().finally(() => {
      this.#draining = false;
    });
  }

  /** The drain itself: one entry at a time, parking when offline or hidden. */
  async #drainLoop(): Promise<void> {
    while (this.#queue.length > 0) {
      if (!this.#gate.open) {
        // The page hid mid-drain: finish nothing new, drop no state, wait to be shown.
        await this.#gate.wait();
        continue;
      }
      if (!this.#isReady()) {
        // Nothing has left and nothing can; the client kicks the drain when ready again.
        return;
      }
      const entry = this.#queue[0];
      if (entry === undefined) {
        return;
      }
      entry.state = 'sending';
      this.#notify(entry);
      let outcome: 'delivered' | 'requeue' | 'failed' = 'requeue';
      let accepted: MessageAccepted | undefined;
      let failure: SdkError | undefined;
      try {
        accepted = await this.#send(entry.conversationId, entry.content, entry.options);
        outcome = 'delivered';
      } catch (cause) {
        failure = cause instanceof SdkError ? cause : new TransportError(String(cause));
        entry.attempts += 1;
        outcome = this.#verdict(failure, entry.attempts);
      }
      if (outcome === 'delivered' && accepted !== undefined) {
        this.#queue.shift();
        entry.state = 'delivered';
        this.#notify(entry);
        entry.resolve(accepted);
      } else if (outcome === 'failed' && failure !== undefined) {
        this.#queue.shift();
        entry.state = 'failed';
        entry.lastError = failure;
        this.#notify(entry);
        entry.reject(failure);
      } else if (failure !== undefined) {
        entry.state = 'queued';
        this.#notify(entry);
        await this.#backoff(failure, entry.attempts);
      }
    }
  }

  /**
   * What a failed attempt means for the entry: retry with backoff, or stop for good.
   *
   * The classes come from the protocol's own taxonomy, so the outbox never hard-codes a code:
   * RateLimit/Server/Federation come back, everything a retry cannot fix fails the entry, and
   * the attempt budget fails it whatever the cause — the visible failure the composer owes the
   * user instead of a retry loop that never ends.
   */
  #verdict(failure: SdkError, attempts: number): 'requeue' | 'failed' {
    if (attempts >= this.#maxAttempts) {
      return 'failed';
    }
    if (failure instanceof TransportError || failure instanceof TimeoutError) {
      return 'requeue';
    }
    if (failure instanceof RemoteError) {
      return isRetryable(failure.code) || errorClass(failure.code) === 'Unknown'
        ? 'requeue'
        : 'failed';
    }
    return 'requeue';
  }

  /**
   * Waits before the next attempt: the server's advice when it gave one, else the exponential
   * curve with jitter — never a tight retry, which is what an overloaded server least needs.
   */
  async #backoff(failure: SdkError, attempts: number): Promise<void> {
    const advised = failure instanceof RemoteError ? failure.retryAfterMs : undefined;
    const exponential = this.#backoffBaseMs * 2 ** Math.max(0, attempts - 1);
    const wait =
      advised !== undefined && advised > 0
        ? advised
        : Math.min(this.#backoffCapMs, exponential) * (0.5 + Math.random() * 0.5);
    if (wait <= 0) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, wait));
  }

  /** Notifies listeners about one entry's change, isolating a throw from the others. */
  #notify(entry: Queued): void {
    const snapshot = this.#snapshot(entry);
    for (const listener of [...this.#listeners]) {
      try {
        listener(snapshot);
      } catch {
        // A UI listener's bug must not stop the drain.
      }
    }
  }

  /** Notifies about the newest entry (an enqueue), with the same isolation. */
  #notifyTail(): void {
    const entry = this.#queue[this.#queue.length - 1];
    if (entry !== undefined) {
      this.#notify(entry);
    }
  }

  /** The public view of one entry: identity and progress, never the content. */
  #snapshot(entry: Queued): OutboxEntry {
    return {
      messageId: entry.messageId,
      conversationId: entry.conversationId,
      state: entry.state,
      attempts: entry.attempts,
    };
  }
}
