/**
 * The verification surface of a direct conversation: the pair numbers, the memory behind the
 * key-change warning, and the views that show both (§47, §164).
 *
 * # What is pinned, and why each part exists
 *
 *   1. **The pair vectors, through the SDK's re-export.** The pair form is a cross-client contract
 *      (every client must render the same two fingerprints as the same string), and Android — which
 *      *defines* it — shipped no vectors of its own. The expected values were derived once with an
 *      independent HKDF call and pinned in the crypto package's suite; asserting them again through
 *      `@migo/sdk` proves the re-export the web client actually imports is the same derivation, not
 *      a near neighbour.
 *   2. **The reconciliation semantics.** The three outcomes a read can produce — first observation,
 *      unchanged, changed — carry rules the warning lives or dies by: a first observation is
 *      recorded silently as the baseline (nothing changed, because nothing was ever seen), and a
 *      changed fingerprint produces a report but *no* record, because the read that detects a
 *      change is the one party that must never clear it. `reconcileSafety` is pure, so the rules are
 *      pinned without a client, a store, or a render.
 *   3. **The store.** The record is keyed by conversation *and* device, because a Migo identity
 *      belongs to a device: a change on the peer's phone is not a change on their laptop. The
 *      round-trip test pins that keying — the same fingerprint under two keys must not collide —
 *      and the forget path.
 *   4. **The views.** The banner's sentence and its Review button, the loading and failure states
 *      (a number shown before the read lands would be a number invented on the spot; a silent
 *      failure would be a verification surface that verifies nothing), the per-device label that
 *      appears only when there is more than one to tell apart, and the acknowledgment button that
 *      exists only while a change is unacknowledged.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { pairSafetyNumber } from '@migo/sdk';
import type { Id } from '@migo/sdk';

import {
  DirectInfoPanel,
  SAFETY_CHANGED_NOTE,
  SAFETY_EXPLANATION,
  SAFETY_WARNING,
  SafetyNumberRow,
  SafetyPanelView,
  SafetyWarningBannerView,
} from '../src/components/direct-info-panel.js';
import type { SafetyState } from '../src/components/direct-info-panel.js';
import { reconcileSafety } from '../src/lib/migo/safety.js';
import type { PeerSafetyNumber, SafetyObservation } from '../src/lib/migo/safety.js';
import {
  clearPeerIdentity,
  loadPeerIdentity,
  savePeerIdentity,
} from '../src/lib/storage/peer-identity-store.js';
import { installFakeIndexedDb } from './support/dom-stubs.js';

/** The 32 bytes a hex string names. */
function unhex(text: string): Uint8Array {
  return new Uint8Array((text.match(/../g) ?? []).map((pair) => parseInt(pair, 16)));
}

/**
 * Static markup with React's entity escapes folded back to plain text, so a sentence containing an
 * apostrophe can be compared with the sentence as it was written.
 */
function textOf(markup: string): string {
  return markup.replaceAll('&#x27;', "'").replaceAll('&quot;', '"').replaceAll('&amp;', '&');
}

// --- the pair vectors, through the re-export the web client imports ----------------------------

test('the SDK re-export derives the pinned pair vector, so every client reads the same string', () => {
  // Derived independently of the implementation (a direct HKDF call) and pinned in the crypto
  // package's suite; this assertion holds the import path to those exact bytes.
  const own = unhex('000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f');
  const peer = unhex('202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f');
  assert.equal(pairSafetyNumber(own, peer), '60088 65422 11574 92697 23746 84297 28533 19234');
  // The two people comparing derive from their own side: the number must not care whose side is
  // "own", including when the first differing byte is above 0x7f and a signed sort would disagree.
  assert.equal(pairSafetyNumber(own, peer), pairSafetyNumber(peer, own));
});

test('the unsigned-sort vector keeps the two sides on one string', () => {
  const own = unhex('ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff');
  const peer = unhex('0001029999999999999999999999999999999999999999999999999999999999');
  assert.equal(pairSafetyNumber(own, peer), '38710 16346 71929 37377 75094 01556 80217 29455');
  assert.equal(pairSafetyNumber(own, peer), pairSafetyNumber(peer, own));
});

// --- the reconciliation semantics ------------------------------------------------------------

const OWN = unhex('000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f');
const FIRST_DEVICE = 'dev_1' as Id;
const SECOND_DEVICE = 'dev_2' as Id;

function observation(deviceId: Id, fingerprint: Uint8Array): SafetyObservation {
  return { deviceId, fingerprint };
}

test('a first observation is the baseline, not a change: recorded silently, no warning', () => {
  const fingerprint = unhex('202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f');
  const { report, firstSeen } = reconcileSafety(
    OWN,
    [observation(FIRST_DEVICE, fingerprint)],
    new Map(),
  );
  assert.equal(report.length, 1);
  assert.equal(report[0]!.changed, false, 'nothing changed, because nothing was ever seen');
  assert.equal(report[0]!.deviceId, FIRST_DEVICE);
  assert.deepEqual(
    firstSeen,
    [observation(FIRST_DEVICE, fingerprint)],
    'the baseline is persisted',
  );
  assert.equal(
    report[0]!.number,
    pairSafetyNumber(OWN, fingerprint),
    'the reported number is the pair derivation itself',
  );
});

test('an unchanged fingerprint reports quiet and writes nothing', () => {
  const fingerprint = unhex('202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f');
  const acknowledged = new Map([[FIRST_DEVICE, fingerprint]]);
  const { report, firstSeen } = reconcileSafety(
    OWN,
    [observation(FIRST_DEVICE, fingerprint)],
    acknowledged,
  );
  assert.equal(report[0]!.changed, false);
  assert.deepEqual(firstSeen, [], 'the store is not rewritten for a match');
});

test('a changed fingerprint warns and is deliberately not recorded by the detecting read', () => {
  const stored = unhex('202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f');
  const current = unhex('808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f');
  const acknowledged = new Map([[FIRST_DEVICE, stored]]);
  const { report, firstSeen } = reconcileSafety(
    OWN,
    [observation(FIRST_DEVICE, current)],
    acknowledged,
  );
  assert.equal(report[0]!.changed, true, 'the change is visible');
  assert.deepEqual(
    firstSeen,
    [],
    'the detecting read must never write the store: the warning would clear itself in the same breath',
  );
  assert.equal(
    report[0]!.number,
    pairSafetyNumber(OWN, current),
    'the reported number is derived from the *current* fingerprint',
  );
});

test('a change on one device is not a change on the other', () => {
  const stable = unhex('202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f');
  const rotated = unhex('808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f');
  const acknowledged = new Map<Id, Uint8Array>([
    [FIRST_DEVICE, stable],
    [SECOND_DEVICE, stable],
  ]);
  const { report, firstSeen } = reconcileSafety(
    OWN,
    [observation(FIRST_DEVICE, stable), observation(SECOND_DEVICE, rotated)],
    acknowledged,
  );
  assert.equal(report[0]!.changed, false, 'the stable device stays quiet');
  assert.equal(report[1]!.changed, true, 'only the rotated device warns');
  assert.deepEqual(firstSeen, []);
});

// --- the store: per conversation, per device --------------------------------------------------

test('the peer-identity record round-trips and is keyed by conversation and device', async () => {
  const fake = installFakeIndexedDb();
  try {
    const fingerprint = unhex('202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f');
    const conversation = 'conv_1' as Id;
    const otherConversation = 'conv_2' as Id;

    assert.equal(
      await loadPeerIdentity(conversation, FIRST_DEVICE),
      undefined,
      'a device never observed reads as undefined',
    );

    await savePeerIdentity(conversation, FIRST_DEVICE, fingerprint);
    assert.deepEqual(
      await loadPeerIdentity(conversation, FIRST_DEVICE),
      fingerprint,
      'the acknowledged fingerprint survives byte for byte',
    );

    assert.equal(
      await loadPeerIdentity(otherConversation, FIRST_DEVICE),
      undefined,
      'another conversation has no record just because this one does — a change is acknowledged per conversation',
    );
    assert.equal(
      await loadPeerIdentity(conversation, SECOND_DEVICE),
      undefined,
      'another device of the same peer has no record just because one does — a change is acknowledged per device',
    );

    await clearPeerIdentity(conversation, FIRST_DEVICE);
    assert.equal(await loadPeerIdentity(conversation, FIRST_DEVICE), undefined);
    await clearPeerIdentity(conversation, FIRST_DEVICE); // absent is a no-op, the house contract
  } finally {
    fake.restore();
  }
});

// --- the views --------------------------------------------------------------------------------

/** A safety state over plain data, for the view tests. */
function safetyState(overrides: Partial<SafetyState> = {}): SafetyState {
  return {
    numbers: null,
    failure: null,
    changed: false,
    acknowledge: () => {},
    retry: () => {},
    ...overrides,
  };
}

function numberEntry(deviceId: Id, changed = false): PeerSafetyNumber {
  return {
    deviceId,
    number: '60088 65422 11574 92697 23746 84297 28533 19234',
    changed,
  };
}

test('the warning banner states the change and offers the one way through it: review', () => {
  const markup = textOf(renderToStaticMarkup(<SafetyWarningBannerView onReview={() => {}} />));
  assert.ok(markup.includes(SAFETY_WARNING), 'the banner carries the warning sentence');
  assert.ok(markup.includes('Review'), 'the banner offers Review, not a silent dismissal');
  assert.match(markup, /role="alert"/, 'it announces itself as an alert');
});

test('a read in flight is a spinner, never a number invented on the spot', () => {
  const markup = renderToStaticMarkup(<SafetyPanelView safety={safetyState()} />);
  assert.match(markup, /role="status"/, 'the loading state is a status, not a guess');
  assert.ok(!markup.includes('60088'), 'no digits render before the read lands');
});

test('a failed read says so and offers the retry it owes', () => {
  const markup = renderToStaticMarkup(
    <SafetyPanelView safety={safetyState({ failure: 'Could not reach the Migo server.' })} />,
  );
  assert.ok(markup.includes('Could not reach the Migo server.'));
  assert.ok(markup.includes('Try again'));
});

test('a single device renders its number without chrome explaining itself', () => {
  const markup = renderToStaticMarkup(
    <SafetyPanelView safety={safetyState({ numbers: [numberEntry(FIRST_DEVICE)] })} />,
  );
  assert.ok(markup.includes('60088 65422 11574 92697 23746 84297 28533 19234'));
  assert.ok(
    !markup.includes('Device'),
    'the per-device label appears only when there is more than one',
  );
  assert.ok(markup.includes(SAFETY_EXPLANATION), 'the comparison sentence travels with the number');
  assert.ok(
    !markup.includes('checked the new number'),
    'no acknowledgment button while nothing changed',
  );
});

test('multiple devices are labelled, a changed one is marked, and the acknowledgment appears', () => {
  const markup = textOf(
    renderToStaticMarkup(
      <SafetyPanelView
        safety={safetyState({
          numbers: [numberEntry(FIRST_DEVICE), numberEntry(SECOND_DEVICE, true)],
          changed: true,
        })}
      />,
    ),
  );
  assert.ok(markup.includes(`Device ${FIRST_DEVICE.slice(0, 8)}`), 'each device is labelled');
  assert.ok(markup.includes(`Device ${SECOND_DEVICE.slice(0, 8)}`));
  assert.ok(markup.includes(SAFETY_CHANGED_NOTE), 'the changed row says what changed');
  assert.ok(markup.includes('checked the new number'), 'the person may acknowledge the change');
});

test('a changed number is distinguished in the row itself, not only by the panel around it', () => {
  const quiet = textOf(
    renderToStaticMarkup(
      <SafetyNumberRow
        deviceId={FIRST_DEVICE}
        number="60088 65422 11574 92697 23746 84297 28533 19234"
        changed={false}
        label={true}
      />,
    ),
  );
  const changed = textOf(
    renderToStaticMarkup(
      <SafetyNumberRow
        deviceId={FIRST_DEVICE}
        number="60088 65422 11574 92697 23746 84297 28533 19234"
        changed={true}
        label={true}
      />,
    ),
  );
  assert.ok(changed.includes(SAFETY_CHANGED_NOTE));
  assert.ok(!quiet.includes(SAFETY_CHANGED_NOTE));
  assert.notEqual(quiet, changed, 'the changed row is visibly different markup');
});

test('the direct conversation panel is the details drawer, titled as the safety surface', () => {
  const markup = renderToStaticMarkup(
    <DirectInfoPanel safety={safetyState({ numbers: [numberEntry(FIRST_DEVICE)] })} />,
  );
  assert.ok(markup.includes('Safety numbers'));
  assert.ok(markup.includes('60088 65422 11574 92697 23746 84297 28533 19234'));
});
