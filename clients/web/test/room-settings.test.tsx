/**
 * What a room's settings section is allowed to show, and to whom.
 *
 * The settings controls are authority, not decoration, so each gate is pinned as a pure predicate —
 * a regression here is a control appearing where the server will only refuse it, or vanishing where
 * it belongs — and the section's rendering is pinned as markup:
 *
 *   1. **The rename — and the slow-mode interval beside it — belongs to an administrator and
 *      above.** The wire gates a settings change on the room's edit permission, which the rank
 *      defaults give an Administrator, a Manager, and the Owner; a Moderator who deletes messages
 *      all day still has no say in the room's name or its pace.
 *   2. **The archive belongs to the owner alone.** The server checks the owner column itself, so no
 *      rank comparison and no global-admin elevation softens this gate.
 *   3. **The effective permission is the override first, the role default second — and archive
 *      consults neither bit.** The wire carries no per-member overrides to the client today (the
 *      roster holds a role and nothing more), so the precedence is pinned here for the day a
 *      surface grows one: a grant hands a bit to a rank the defaults refuse, a deny takes it from
 *      a rank the defaults give it, and the owner's archive is the owner column, not a bit.
 *   4. **A role change needs the manage permission and a member strictly below the viewer.** The
 *      items offered are the wire's real ranks — not the badge's collapsed "Admin" — and stop one
 *      rung below the viewer's own, because the server refuses a grant at or above it.
 *   5. **A viewer with no settings sees nothing at all**, not a panel of disabled inputs.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import { renderToStaticMarkup } from 'react-dom/server';

import { RoomRole } from '@migo/sdk';
import type { Id, RosterEntry } from '@migo/sdk';

import {
  RosterList,
  RoomSettings,
  SLOW_MODE_MAX_SECONDS,
  canSetRole,
  effectivePermission,
  parseSlowModeSeconds,
  settableRoles,
} from '../src/components/room-info-panel.js';

const JOINED = Date.parse('2026-08-26T12:00:00Z');

function roster(accountId: string, role: number): RosterEntry {
  return { accountId: accountId as Id, role, joinedAt: JOINED };
}

test('the rename — and the slow-mode interval beside it — belongs to an administrator and above', () => {
  assert.equal(effectivePermission({ role: RoomRole.Owner }, 'edit'), true);
  assert.equal(effectivePermission({ role: RoomRole.Manager }, 'edit'), true);
  assert.equal(effectivePermission({ role: RoomRole.Admin }, 'edit'), true);
  assert.equal(effectivePermission({ role: RoomRole.Moderator }, 'edit'), false);
  assert.equal(effectivePermission({ role: RoomRole.Helper }, 'edit'), false);
  assert.equal(effectivePermission({ role: RoomRole.Member }, 'edit'), false);
  // Unknown — not on the roster, or a value a newer node sent — offers nothing.
  assert.equal(effectivePermission({ role: RoomRole.Unknown }, 'edit'), false);
});

test('the archive belongs to the owner alone', () => {
  assert.equal(effectivePermission({ role: RoomRole.Owner }, 'archive'), true);
  assert.equal(effectivePermission({ role: RoomRole.Manager }, 'archive'), false);
  // A global admin moderates any room, but the server refuses an archive from anyone but the
  // owner, so no elevation softens this gate the way it softens a sanction.
  assert.equal(effectivePermission({ role: RoomRole.Admin }, 'archive'), false);
  assert.equal(effectivePermission({ role: RoomRole.Member }, 'archive'), false);
});

test('a per-member override is the effective permission, winning over the role default both ways', () => {
  // A grant hands the edit bit to a rank the defaults refuse — the moderator a manager trusted
  // with the room's name, without the rank that would also let them ban.
  assert.equal(
    effectivePermission({ role: RoomRole.Moderator, overrides: { edit: true } }, 'edit'),
    true,
  );
  // A deny takes it from a rank the defaults give it — the administrator under a correction.
  assert.equal(
    effectivePermission({ role: RoomRole.Owner, overrides: { edit: false } }, 'edit'),
    false,
  );
  // The manage bit answers the same way, and an override for one action leaves the others on
  // their role defaults: the permissions are per-bit, not a blanket.
  assert.equal(
    effectivePermission({ role: RoomRole.Moderator, overrides: { manage: true } }, 'manage'),
    true,
  );
  assert.equal(
    effectivePermission({ role: RoomRole.Moderator, overrides: { manage: true } }, 'edit'),
    false,
  );
  // Archive is the owner column, not a permission bit: no override grants it to another member,
  // and none is consulted for it — not even a deny takes the ending of the room from its owner.
  assert.equal(
    effectivePermission({ role: RoomRole.Manager, overrides: { archive: true } }, 'archive'),
    false,
  );
  assert.equal(
    effectivePermission({ role: RoomRole.Owner, overrides: { archive: false } }, 'archive'),
    true,
  );
});

test('a role change needs the manage permission and a member strictly below the viewer', () => {
  // A manager may re-rank an administrator on down.
  assert.equal(canSetRole(RoomRole.Manager, RoomRole.Admin, false), true);
  assert.equal(canSetRole(RoomRole.Manager, RoomRole.Helper, false), true);
  // Not a peer, not the owner, not their own row.
  assert.equal(canSetRole(RoomRole.Manager, RoomRole.Manager, false), false);
  assert.equal(canSetRole(RoomRole.Manager, RoomRole.Owner, false), false);
  assert.equal(canSetRole(RoomRole.Manager, RoomRole.Member, true), false);
  // A moderator holds the moderation bits but not the manage one, so no role items at all.
  assert.equal(canSetRole(RoomRole.Moderator, RoomRole.Member, false), false);
  // The owner may re-rank anyone but themselves.
  assert.equal(canSetRole(RoomRole.Owner, RoomRole.Manager, false), true);
  assert.equal(canSetRole(RoomRole.Owner, RoomRole.Owner, false), false);
});

test('the roles a viewer may grant stop one rung below their own, and never name the owner', () => {
  assert.deepEqual(
    settableRoles(RoomRole.Owner).map((role) => role.value),
    [RoomRole.Member, RoomRole.Helper, RoomRole.Moderator, RoomRole.Admin, RoomRole.Manager],
  );
  assert.deepEqual(
    settableRoles(RoomRole.Manager).map((role) => role.value),
    [RoomRole.Member, RoomRole.Helper, RoomRole.Moderator, RoomRole.Admin],
  );
  // Nobody grants Owner — ownership moves by transfer, not by a role change — and a plain
  // member grants nothing at all. (A moderator's ladder holds Member and Helper, but a
  // moderator holds no manage permission, so {@link canSetRole} keeps those items off their
  // roster entirely; the ladder here is only the ranks the server would accept.)
  assert.deepEqual(
    settableRoles(RoomRole.Moderator).map((role) => role.value),
    [RoomRole.Member, RoomRole.Helper],
  );
  assert.deepEqual(settableRoles(RoomRole.Member), []);
  // The labels are the wire's real ranks, not the badge's collapsed words: a grant must name
  // what the server will store.
  assert.deepEqual(
    settableRoles(RoomRole.Admin).map((role) => role.label),
    ['Member', 'Helper', 'Moderator'],
  );
});

test('an owner sees the rename fields seeded with the room, and the archive control', () => {
  const markup = renderToStaticMarkup(
    <RoomSettings
      name="Observatory"
      topic="What is above us"
      canEdit={true}
      canArchive={true}
      onApply={() => {}}
      onArchive={() => {}}
    />,
  );
  assert.ok(markup.includes('aria-label="Room name"'), 'the rename field is missing');
  assert.ok(markup.includes('aria-label="Room topic"'), 'the topic field is missing');
  assert.ok(markup.includes('value="Observatory"'), 'the name field lost its seed');
  assert.ok(markup.includes('value="What is above us"'), 'the topic field lost its seed');
  assert.ok(markup.includes('Archive Room'), 'the archive control is missing');
  // An unchanged screen is not a change: the save button starts disabled, so a submit that
  // changed nothing cannot even be asked for — the server would drop every field anyway.
  assert.ok(
    markup.includes('disabled'),
    'the save button must start disabled on an unchanged screen',
  );
});

test('a viewer with neither control sees no settings at all, not a disabled husk', () => {
  const markup = renderToStaticMarkup(
    <RoomSettings
      name="Observatory"
      canEdit={false}
      canArchive={false}
      onApply={() => {}}
      onArchive={() => {}}
    />,
  );
  assert.equal(markup, '', 'a plain member must be shown no settings section');
});

test('an administrator sees the rename but not the archive', () => {
  const markup = renderToStaticMarkup(
    <RoomSettings
      name="Observatory"
      canEdit={true}
      canArchive={false}
      onApply={() => {}}
      onArchive={() => {}}
    />,
  );
  assert.ok(markup.includes('aria-label="Room name"'), 'the rename the rank admits is missing');
  assert.ok(!markup.includes('Archive Room'), 'the archive leaked onto a non-owner’s panel');
});

test('a slow-mode interval seeds from the room, is bounded by the server’s hour, and can be turned off', () => {
  const markup = renderToStaticMarkup(
    <RoomSettings
      name="Observatory"
      topic="What is above us"
      slowModeSec={30}
      canEdit={true}
      canArchive={false}
      onApply={() => {}}
      onArchive={() => {}}
    />,
  );
  assert.ok(markup.includes('aria-label="Slow mode seconds"'), 'the slow-mode field is missing');
  assert.ok(markup.includes('value="30"'), 'the slow-mode field lost its seed');
  assert.ok(markup.includes('Turn Off'), 'the clear affordance is missing');
  // The server refuses an interval above an hour, so the field never asks for one.
  assert.ok(
    markup.includes(`max="${SLOW_MODE_MAX_SECONDS}"`),
    'the slow-mode field must carry the server’s ceiling',
  );
  // A room with no interval seeds zero — the field's honest "off", not a blank a save would read.
  const offMarkup = renderToStaticMarkup(
    <RoomSettings
      name="Observatory"
      canEdit={true}
      canArchive={false}
      onApply={() => {}}
      onArchive={() => {}}
    />,
  );
  assert.ok(offMarkup.includes('value="0"'), 'an unset interval must seed the field at zero');
  // The field is the edit permission's, like the rename beside it: a viewer without it sees none.
  const bareMarkup = renderToStaticMarkup(
    <RoomSettings
      name="Observatory"
      canEdit={false}
      canArchive={false}
      onApply={() => {}}
      onArchive={() => {}}
    />,
  );
  assert.ok(
    !bareMarkup.includes('aria-label="Slow mode seconds"'),
    'the slow-mode field leaked onto a viewer the edit permission excludes',
  );
});

test('a slow-mode field parses only whole seconds the server would take', () => {
  // The honest answers: an interval, and zero as slow mode off.
  assert.equal(parseSlowModeSeconds('30'), 30);
  assert.equal(parseSlowModeSeconds('0'), 0);
  assert.equal(parseSlowModeSeconds(String(SLOW_MODE_MAX_SECONDS)), SLOW_MODE_MAX_SECONDS);
  // Whitespace around a number is a typist's habit, not a different answer.
  assert.equal(parseSlowModeSeconds(' 45 '), 45);
  // Everything else is refused here rather than sent to be refused there: an empty field, text,
  // a negative wait, a decimal (the wire truncates, so a half-second is off, not a shorter
  // interval — the field must not pretend otherwise), and anything past the server's hour.
  assert.equal(parseSlowModeSeconds(''), null);
  assert.equal(parseSlowModeSeconds('soon'), null);
  assert.equal(parseSlowModeSeconds('-5'), null);
  assert.equal(parseSlowModeSeconds('10.5'), null);
  assert.equal(parseSlowModeSeconds(String(SLOW_MODE_MAX_SECONDS + 1)), null);
});

test('a manager sees the role items on a lower-ranked member, and none on the owner or their own row', () => {
  const markup = renderToStaticMarkup(
    <RosterList
      entries={[
        roster('user_owner', RoomRole.Owner),
        roster('user_me', RoomRole.Manager),
        roster('user_bob', RoomRole.Member),
      ]}
      profiles={
        new Map([
          ['user_owner' as Id, { displayName: 'Ada' }],
          ['user_me' as Id, { displayName: 'Me' }],
          ['user_bob' as Id, { displayName: 'Bob' }],
        ])
      }
      viewerId={'user_me' as Id}
      viewerRole={RoomRole.Manager}
      onSetRole={() => {}}
    />,
  );
  // Bob's row offers every rank below the manager's except the one he already holds.
  assert.ok(markup.includes('Make Helper'), 'a grantable rank is missing');
  assert.ok(markup.includes('Make Moderator'), 'a grantable rank is missing');
  assert.ok(markup.includes('Make Administrator'), 'a grantable rank is missing');
  // The rank Bob already holds is not offered: granting it is a silent no-op, so the item would
  // be a button that does nothing.
  assert.ok(!markup.includes('Make Member'), 'the rank the member already holds is offered');
  // A grant at or above the actor's own rank is refused by the server, and Owner moves by
  // transfer — neither may appear as an item.
  assert.ok(!markup.includes('Make Manager'), 'a grant at the actor’s own rank appeared');
  assert.ok(!markup.includes('Make Owner'), 'ownership appeared as a grantable rank');
  // Exactly one row bears the items: Bob's — never the owner's, never the viewer's own. The
  // role items are the only menu items this roster offers (no profile, gift, or sanction
  // handlers were supplied), so one menuitem per offered rank is the count to pin.
  assert.equal(
    (markup.match(/role="menuitem"/g) ?? []).length,
    3,
    'the role items leaked onto a row that must not carry them',
  );
});
