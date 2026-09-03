/**
 * Maps an SDK error to a short, user-facing message.
 *
 * A {@link RemoteError} carries a stable `symbol` and an optional human `message`. The server only
 * fills that message in when it has explicitly marked one safe to disclose; by default it withholds
 * it (section 161), so the message arrives empty and the SDK folds the empty value into the bare
 * `symbol` — `error.message` becomes e.g. `UNAUTHENTICATED`. That symbol is internal vocabulary: it
 * must never be shown to a user, both because it leaks machine codes into the UI and because it would
 * make two deliberately-withheld errors — a missing resource versus a privacy-restricted one
 * (section 180) — distinguishable when the server took care to make them identical.
 *
 * So the order of answers is: the server's disclosed text first (it is the authority, and it can say
 * things a static table cannot — a retry interval, a limit's size), then the table below for the
 * conditions a person actually hits and that are safe to name in our own words — the symbol is the
 * stable contract, so a friendlier line than the raw symbol is worth writing for each — and a single
 * generic line for everything else. Symbols that would split a pair the server deliberately collapsed
 * (NOT_FOUND versus PRIVACY_RESTRICTED, the section 180 rule) deliberately have no table entry: both
 * fall to the same generic line. Transport and timeout failures are rephrased for a person. Nothing
 * here logs the error object, which keeps tokens and payloads out of the console by construction.
 */

import { RemoteError, SdkError, TimeoutError, TransportError } from '@migo/sdk';

/**
 * The conditions worth their own words when the server stayed silent. Every entry must be safe to
 * name: it may reveal nothing about other accounts (no enumeration) and must not contradict the
 * pair-collapses the server performs — which is why NOT_FOUND and PRIVACY_RESTRICTED are absent.
 */
const LINES: Readonly<Record<string, string>> = {
  // The auth gates a person meets at the door.
  INVALID_CREDENTIALS: 'That username or passphrase is not right. Check them and try again.',
  USERNAME_TAKEN: 'That username is already taken. Try another one.',
  USERNAME_RESERVED: 'That username is reserved. Try another one.',
  WEAK_PASSWORD: 'That passphrase is too easy to guess. Make it longer and more varied.',
  ACCOUNT_SUSPENDED: 'This account has been suspended.',
  AUTH_LOCKED: 'Sign-in is temporarily locked after repeated failures. Wait a while and try again.',
  INVALID_CAPTCHA: 'That captcha answer did not match. Ask for a new challenge and try again.',
  CAPTCHA_EXPIRED: 'The captcha expired. Ask for a new challenge and try again.',
  CAPTCHA_REQUIRED: 'This needs a captcha answer first. Complete the captcha and try again.',
  REAUTHENTICATION_REQUIRED: 'For safety, sign in again before doing that.',
  // The rate family: the honest answer is "wait".
  RATE_LIMITED: 'Too many requests too quickly. Wait a moment and try again.',
  QUOTA_EXCEEDED: 'You have reached your quota for now. Try again later.',
  SLOW_MODE_ACTIVE: 'Slow mode is on here. Wait a moment before sending again.',
  TOO_MANY_SESSIONS: 'Too many active sessions. Revoke one in Settings, then try again.',
  UPLOAD_LIMIT_EXCEEDED: 'That file is too large for the server to accept.',
  // What a conversation can say no to.
  PERMISSION_DENIED: 'You do not have permission to do that.',
  NOT_A_MEMBER: 'You are not a member of this conversation any more.',
  MUTED: 'You are muted in this conversation. You can speak again once the mute lifts.',
  BANNED: 'You have been banned from this room.',
  BLOCKED_BY_USER: 'That person does not receive messages from you.',
  VOTE_TARGET_IMMUNE: 'A vote cannot remove that person — they run this room or group.',
  VOTE_ALREADY_OPEN: 'A removal vote is already running. Add your voice to it instead.',
  ROOM_FULL: 'This room is full right now. Try again later, or find another room.',
  ROOM_ARCHIVED: 'This room is archived and read-only.',
  GROUP_FULL: 'This group has reached its member limit.',
  INSUFFICIENT_BALANCE: 'Not enough balance for that. Top up your wallet and try again.',
  FEATURE_DISABLED: 'That feature has been switched off on this server.',
};

/** What a withheld error may say: honest about the shape of the refusal, specific about nothing. */
const GENERIC_REMOTE =
  'The server turned that down. It is not your connection — check what you were doing and try again, or come back later.';

export function friendlyError(error: unknown): string {
  if (error instanceof RemoteError) {
    // The SDK's JS message composes `symbol: human text`, so the symbol is stripped here rather
    // than compared: "RATE_LIMITED: Too many requests. Retry in 5 s" reaches a person as "Too
    // many requests. Retry in 5 s". When the server disclosed no human text at all, the bare
    // symbol is never surfaced — the table speaks for the conditions it knows, and everything
    // else falls to the one generic line, so a withheld error stays unreadable and pairs the
    // server collapsed stay indistinguishable.
    const human = error.message.startsWith(`${error.symbol}: `)
      ? error.message.slice(error.symbol.length + 2)
      : error.message;
    if (human && human !== error.symbol) {
      return human;
    }
    return LINES[error.symbol] ?? GENERIC_REMOTE;
  }
  if (error instanceof TimeoutError) {
    return 'The server took too long to respond. Check your connection and try again.';
  }
  if (error instanceof TransportError) {
    return 'Could not reach the Migo server. Check your connection and try again.';
  }
  if (error instanceof SdkError) {
    return error.message;
  }
  return 'Something went wrong. Please try again.';
}
