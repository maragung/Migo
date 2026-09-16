/**
 * The moderation domain: what a filing carries and what the four conveniences send.
 *
 * Like the sibling domains, every test drives a real {@link Rpc} over the {@link
 * RecordingTransport} double, so what the domain *sent* is decoded back out of the recorded frame
 * body against the generated codec — a mismatched struct fails to decode or decodes wrong, which is
 * the whole reason the assertion is made on bytes rather than on a captured object.
 *
 * Three assertions carry protocol weight beyond shape:
 *
 *   1. **The subject kind is the wire's, not the store's.** `REPORT_CREATE` numbers a subject 0
 *      user, 1 message, 2 room, 3 bot. The node's *storage* vocabulary numbers a bot 4 and a media
 *      object 3, so a client that echoed the storage numbers would file every bot report as a media
 *      report. The test pins all four wire values.
 *   2. **A message report names one id and nothing else.** The wire has a single `subject_id` and a
 *      report row stores a single id, so a caller must not be able to smuggle a conversation into
 *      it — the body decodes exactly as `{subjectKind, subjectId, reason}`, no more.
 *   3. **An over-long note is refused locally.** The check exists so a rejected call does not spend
 *      the frame or the report's cost, which means it has to fire *before* anything is sent; the
 *      test asserts the transport stayed empty.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  decodeBody,
  encodeBody,
  ModerationDomain,
  ReportReason,
  ReportSubject,
  Rpc,
} from '../src/index.js';
import { OP } from '@migo/protocol';
import { decodeReportFile, encodeAcknowledged, encodeModerationEvent } from '@migo/protocol';
import type { ReportFile } from '@migo/protocol';

import { RecordingTransport, idOf } from './harness.js';

/** Builds a domain over one recording transport, with per-opcode canned replies. */
function rig(replies: Map<number, (body: Uint8Array) => Uint8Array>): {
  transport: RecordingTransport;
  moderation: ModerationDomain;
} {
  const transport = new RecordingTransport();
  transport.reply = (opcode, body) => (replies.get(opcode) ?? (() => new Uint8Array()))(body);
  const rpc = new Rpc(transport.asTransport());
  return { transport, moderation: new ModerationDomain(rpc) };
}

/** A rig whose every `REPORT_CREATE` is acknowledged. */
function ackingRig(): ReturnType<typeof rig> {
  return rig(new Map([[OP.REPORT_CREATE, () => encodeBody(encodeAcknowledged, { ok: true })]]));
}

const TARGET = idOf(31);
const CASE = idOf(32);

/** The body of the frame the domain sent, decoded as the wire's own `ReportFile`. */
function sent(transport: RecordingTransport, index = 0): ReportFile {
  const frame = transport.sent[index];
  assert.ok(frame !== undefined, `no frame was sent at index ${index}`);
  assert.equal(frame.opcode, OP.REPORT_CREATE);
  return decodeBody(decodeReportFile, frame.body ?? new Uint8Array());
}

test('moderation: reportUser files a report whose subject kind is the wire zero', async () => {
  const { transport, moderation } = ackingRig();
  await moderation.reportUser(TARGET, ReportReason.Harassment);
  const body = sent(transport);
  assert.equal(body.subjectKind, ReportSubject.User);
  assert.equal(body.subjectKind, 0, 'a user is subject kind 0 on the wire');
  assert.equal(body.subjectId, TARGET);
  assert.equal(body.reason, ReportReason.Harassment);
  assert.equal(body.note, undefined, 'no note was passed, so the optional field is absent');
});

test('moderation: reportMessage names one id and cannot smuggle a conversation', async () => {
  const { transport, moderation } = ackingRig();
  await moderation.reportMessage(TARGET, ReportReason.Spam);
  const body = sent(transport);
  assert.equal(body.subjectKind, 1, 'a message is subject kind 1 on the wire');
  assert.deepEqual(Object.keys(body).sort(), ['reason', 'subjectId', 'subjectKind']);
});

test('moderation: reportRoom and reportBot use the wire kinds, not the store ones', async () => {
  const { transport, moderation } = ackingRig();
  await moderation.reportRoom(TARGET, ReportReason.Scam);
  await moderation.reportBot(TARGET, ReportReason.BotAbuse);
  assert.equal(sent(transport, 0).subjectKind, 2, 'a room is subject kind 2 on the wire');
  // The store numbers a bot 4 and a media object 3; the wire numbers a bot 3 and has no media
  // kind at all. Sending the storage number would file this report as being about a media object.
  assert.equal(sent(transport, 1).subjectKind, 3, 'a bot is subject kind 3 on the wire');
  assert.equal(transport.sent.length, 2);
});

test('moderation: a note rides the wire, trimmed of nothing the caller did not send', async () => {
  const { transport, moderation } = ackingRig();
  await moderation.report({ kind: ReportSubject.Room, id: TARGET }, ReportReason.MaliciousLink, {
    note: 'the pinned link is a phishing page',
  });
  const body = sent(transport);
  assert.equal(body.note, 'the pinned link is a phishing page');
  assert.equal(body.reason, ReportReason.MaliciousLink);
});

test('moderation: an over-long note is refused before anything is sent', async () => {
  const { transport, moderation } = ackingRig();
  await assert.rejects(
    () => moderation.reportUser(TARGET, ReportReason.Other, { note: 'x'.repeat(501) }),
    RangeError,
  );
  assert.equal(transport.sent.length, 0, 'a locally refused report must not reach the wire');
  // The boundary itself is accepted, because the node's own limit is inclusive of 500.
  await moderation.reportUser(TARGET, ReportReason.Other, { note: 'x'.repeat(500) });
  assert.equal(transport.sent.length, 1);
});

test('moderation: every reason code is the store vocabulary value', () => {
  // These are stored as given, so a renumbering here would silently rewrite the meaning of every
  // report already in a queue. The list is the warden's own `Reason` ordering.
  assert.deepEqual(
    Object.entries(ReportReason).filter(([, value]) => typeof value === 'number'),
    [
      ['Spam', 0],
      ['Flood', 1],
      ['Scam', 2],
      ['MaliciousLink', 3],
      ['Harassment', 4],
      ['HateSpeech', 5],
      ['SexualContent', 6],
      ['Violence', 7],
      ['SelfHarm', 8],
      ['Impersonation', 9],
      ['ChildSafety', 10],
      ['BotAbuse', 11],
      ['Other', 12],
    ],
  );
});

test('moderation: onModerationEvent delivers decoded cases once started, and stops cleanly', () => {
  const { transport, moderation } = rig(new Map());
  const seen: string[] = [];
  moderation.onModerationEvent((event) => seen.push(event.state));

  moderation.start();
  transport.emit(
    OP.MODERATION_EVENT,
    encodeBody(encodeModerationEvent, { caseId: CASE, action: 1, state: 'warned' }),
  );
  assert.deepEqual(seen, ['warned']);

  moderation.stop();
  transport.emit(
    OP.MODERATION_EVENT,
    encodeBody(encodeModerationEvent, { caseId: CASE, action: 0, state: 'no_action' }),
  );
  assert.equal(seen.length, 1, 'an event after stop() must not be delivered');
});
