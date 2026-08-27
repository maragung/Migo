/**
 * What a presence label is allowed to reveal.
 *
 * Section 180 has a presence dimension. The protocol carries a `PresenceState.Invisible` — an account
 * that is connected but has chosen to appear offline — and an `Unknown` state, alongside the ordinary
 * Online / Away / Busy / Offline. `presenceLabel` turns a state into the words a user reads, so it is
 * the exact place a privacy state could leak: if it ever rendered "Invisible" (or "Hidden"), an
 * observer would learn that a user is deliberately hiding, which is precisely what invisibility is
 * meant to prevent. Equally, a user for whom we hold no presence at all must read the same as any
 * other unrevealed state, not as some distinct "unknown" badge.
 *
 * These tests pin the four disclosable states to their words and assert the whole function can never
 * emit a privacy-revealing term — for the hidden states, for an absent value, or for a garbage value
 * from a malformed frame. A regression that added a `case Invisible: return 'Invisible'` would sail
 * through every rendering test while quietly outing every invisible user.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { PresenceState } from '@migo/sdk';

import { presenceLabel } from '../src/lib/migo/use-presence.js';

test('each disclosable presence state maps to its own non-empty word', () => {
  const labels = {
    online: presenceLabel(PresenceState.Online),
    away: presenceLabel(PresenceState.Away),
    busy: presenceLabel(PresenceState.Busy),
    offline: presenceLabel(PresenceState.Offline),
  };
  assert.equal(labels.online, 'Online');
  assert.equal(labels.away, 'Away');
  assert.equal(labels.busy, 'Busy');
  assert.equal(labels.offline, 'Offline');
  // The four are distinct, or the badge would be ambiguous.
  assert.equal(new Set(Object.values(labels)).size, 4);
});

test('an absent presence reads as an empty label, not as a distinct "unknown" state', () => {
  assert.equal(presenceLabel(undefined), '');
  assert.equal(presenceLabel(PresenceState.Unknown), '');
});

test('an invisible user is never outed by the label; they read as no presence at all', () => {
  // The whole point of Invisible is to be indistinguishable from having no presence shown.
  assert.equal(presenceLabel(PresenceState.Invisible), '');
  assert.equal(presenceLabel(PresenceState.Invisible), presenceLabel(undefined));
});

test('no presence state, valid or malformed, ever emits a privacy-revealing word', () => {
  const states: Array<PresenceState | undefined> = [
    undefined,
    PresenceState.Unknown,
    PresenceState.Offline,
    PresenceState.Online,
    PresenceState.Away,
    PresenceState.Busy,
    PresenceState.Invisible,
    // A value outside the enum, as a corrupted frame might carry.
    99 as PresenceState,
  ];
  for (const state of states) {
    const label = presenceLabel(state);
    assert.ok(!/invisible|hidden|block|restrict|unknown/i.test(label), `leaked via "${label}"`);
  }
});
