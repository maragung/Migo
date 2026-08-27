/**
 * The formatting helpers, and the limits baked into them.
 *
 * These functions are pure and unglamorous, which is exactly why their edges rot unnoticed. Three of
 * them encode a boundary that the brief treats as a limit to be enforced at its edge, and a one-off
 * error in any of them is the kind of bug that ships: `formatRelative` switches between `now`, minutes,
 * hours, days, and an absolute date at fixed thresholds; `initials` promises *at most two* letters and
 * would spill to three the moment the "first plus last" logic regresses to "first N"; and `hueFromId`
 * must always land inside a single 0–359 turn of the colour wheel or an avatar's colour becomes
 * `hsl(400 …)`, which is undefined. So the tests below pin each threshold with a value at the limit and
 * a value just past it, and assert `hueFromId` stays in range and stays stable for a given id — the
 * property the whole point of the function (a consistent colour across sessions) depends on.
 */

import assert from 'node:assert/strict';
import test from 'node:test';

import {
  formatClock,
  formatDayLabel,
  formatRelative,
  hueFromId,
  initials,
} from '../src/lib/format.js';

const NOW = Date.parse('2026-08-26T12:00:00Z');
const ago = (ms: number): number => NOW - ms;
const SECOND = 1_000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

test('a timestamp inside the last 45 seconds reads as "now"', () => {
  assert.equal(formatRelative(NOW, NOW), 'now');
  assert.equal(formatRelative(ago(44 * SECOND), NOW), 'now');
});

test('a future timestamp is clamped to "now" rather than showing a negative age', () => {
  assert.equal(formatRelative(NOW + 5 * MINUTE, NOW), 'now');
});

test('at 45 seconds the label crosses from "now" to minutes', () => {
  // The limit is `seconds < 45`; the first second past it must round up to a minute, not stay "now".
  assert.equal(formatRelative(ago(45 * SECOND), NOW), '1m');
});

test('minutes are shown up to the last minute before an hour, then hours take over', () => {
  assert.equal(formatRelative(ago(5 * MINUTE), NOW), '5m');
  assert.equal(formatRelative(ago(59 * MINUTE), NOW), '59m');
  // 60 minutes is not `< 60`, so it becomes the first hour.
  assert.equal(formatRelative(ago(60 * MINUTE), NOW), '1h');
});

test('hours are shown up to the last hour before a day, then days take over', () => {
  assert.equal(formatRelative(ago(3 * HOUR), NOW), '3h');
  assert.equal(formatRelative(ago(23 * HOUR), NOW), '23h');
  assert.equal(formatRelative(ago(24 * HOUR), NOW), '1d');
});

test('days are shown up to six, and a week or more falls through to an absolute date', () => {
  assert.equal(formatRelative(ago(2 * DAY), NOW), '2d');
  assert.equal(formatRelative(ago(6 * DAY), NOW), '6d');
  // Seven days is not `< 7`: the coarse relative label gives way to a real date.
  const week = formatRelative(ago(7 * DAY), NOW);
  assert.ok(!/^\d+d$/.test(week), `expected a date, got ${week}`);
  assert.ok(week.length > 0);
});

test('initials yield at most two letters, first and last, however many words the name has', () => {
  assert.equal(initials('Alice'), 'A');
  assert.equal(initials('Alice Wonderland'), 'AW');
  // Three words is the case that would break a naive "take the first two" implementation.
  assert.equal(initials('alice b carol'), 'AC');
  assert.equal(initials('John   Smith'), 'JS');
});

test('initials degrade to a single placeholder when there is no name to take them from', () => {
  assert.equal(initials(''), '?');
  assert.equal(initials('   '), '?');
});

test('initials are always upper-cased regardless of the source casing', () => {
  assert.equal(initials('édouard manet'), 'ÉM');
  assert.equal(initials('x'), 'X');
});

test('a hue derived from an id always lands inside a single 0-359 colour turn', () => {
  const ids = ['', 'a', 'conv_0001', 'conv_0002', 'ZZZZZZZZ', '💬', 'a'.repeat(500)];
  for (const id of ids) {
    const hue = hueFromId(id);
    assert.ok(Number.isInteger(hue), `hue for ${id} is not an integer`);
    assert.ok(hue >= 0 && hue < 360, `hue ${hue} for ${id} is out of range`);
  }
});

test('a hue is stable for a given id and generally differs between ids', () => {
  assert.equal(hueFromId('conv_0001'), hueFromId('conv_0001'));
  assert.notEqual(hueFromId('conv_0001'), hueFromId('conv_9999'));
});

test('the clock is a stable, colon-separated time for a given instant', () => {
  const a = formatClock(NOW);
  assert.match(a, /\d{1,2}:\d{2}/);
  assert.equal(formatClock(NOW), a);
  assert.notEqual(formatClock(NOW), formatClock(NOW + 37 * MINUTE));
});

test('the day label names today and yesterday, and dates anything older', () => {
  assert.equal(formatDayLabel(Date.now()), 'Today');
  assert.equal(formatDayLabel(Date.now() - DAY), 'Yesterday');
  const old = formatDayLabel(Date.parse('2001-02-03T12:00:00Z'));
  assert.notEqual(old, 'Today');
  assert.notEqual(old, 'Yesterday');
  assert.ok(old.length > 0);
});
