'use client';

/**
 * The Bots window: the developer surface for the accounts you run.
 *
 * Section 41 asks for a bot surface a developer can build against and this client had none of it:
 * the SDK could register a bot, rotate its token, pause it, and set its permissions, and a person
 * using the app could do none of those things. This window is that surface, and it is deliberately
 * a window rather than a tab — a bot is not a friendship and not a room, so the Friends and Rooms
 * tabs are the wrong homes for it, and the shell already has a place for an account-scoped tool.
 *
 * Three things here are shaped by the wire rather than by taste:
 *
 *   1. **A token is shown once, and this panel treats it as a one-time reveal.** Only a keyed tag
 *      is stored on the node, so a reply that is lost is a credential that is lost. The token
 *      therefore never enters the list: it appears in one card, above everything, with the copy
 *      button and the warning, and rotating again is what a person does when they missed it — not
 *      a "show token" that would have to be absent to be honest.
 *   2. **Permissions are replaced, never merged.** The editor holds a whole set, and Save sends
 *      that whole set, which is what makes two windows open on the same bot agree rather than
 *      interleave into a union neither person chose.
 *   3. **A paused bot is rendered from the flag the node sent, not inferred.** Both tagged fields
 *      may be absent on a node that predates them, and this panel says "this server did not say"
 *      rather than drawing an unpaused bot with no permissions — the difference between an old
 *      node and an off bot is one a management screen has to keep.
 *
 * Registering and rotation are the only two paths in the whole client that hand back a secret, so
 * both of them land in the same reveal card rather than in two places that could drift apart.
 */

import { useCallback, useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';

import { BOT_SCOPES, NO_SCOPES } from '@migo/sdk';
import type { BotScope, BotView, Id } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Icon } from './icons.js';
import { EmptyState, Skeleton } from './states.js';

/**
 * What each permission means, in the words a person deciding would use.
 *
 * The slugs themselves are the wire's vocabulary and never change, but nobody granting authority
 * should have to read `send_announcements` and guess at blast radius: the label is the verb, the
 * hint is what holding it lets a bot do.
 */
const SCOPE_LABEL: Readonly<Record<BotScope, string>> = {
  read_messages: 'Read messages',
  send_messages: 'Send messages',
  moderate: 'Moderate',
  manage_games: 'Manage games',
  read_members: 'Read members',
  send_announcements: 'Send announcements',
};

const SCOPE_HINT: Readonly<Record<BotScope, string>> = {
  read_messages: 'See the content of messages it is a member of.',
  send_messages: 'Post messages as itself, on the ordinary messaging path.',
  moderate: 'Act on members and content in the rooms it belongs to.',
  manage_games: 'Start, join, and end games in its conversations.',
  read_members: 'See who is in its conversations and rooms.',
  send_announcements: 'Post to a room regardless of who has muted it.',
};

/** The token in hand, and which bot it belongs to — the card that must not be missed. */
interface Reveal {
  botId: Id;
  name: string;
  token: string;
  /** Rotation, so the card can say what just happened rather than only what to do. */
  rotated: boolean;
}

/** The panel. */
export function BotsPanel(): ReactNode {
  const { client } = useMigo();
  const [bots, setBots] = useState<BotView[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reveal, setReveal] = useState<Reveal | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  /** The bot whose permissions editor is open, if any. */
  const [editing, setEditing] = useState<Id | null>(null);
  /** The bot whose rotation is one click from happening. */
  const [confirming, setConfirming] = useState<Id | null>(null);
  /** The last few things the node said about a bot, newest first. */
  const [events, setEvents] = useState<ReadonlyArray<{ botId: Id; event: string }>>([]);

  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client.bots
      .list()
      .then((listed) => {
        if (!cancelled) {
          setBots(listed);
          setError(null);
        }
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setError(friendlyError(cause));
        }
      });
    // The node's word about a bot is the owner's to hear — a webhook that failed to deliver, for
    // instance — and it is the one part of this window that moves without a click.
    const off = client.bots.onBotEvent((event) => {
      if (!cancelled) {
        setEvents((seen) => [{ botId: event.botId, event: event.event }, ...seen].slice(0, 5));
      }
    });
    return () => {
      cancelled = true;
      off();
    };
  }, [client]);

  /** Runs one management call, folding whatever it returns back into the list. */
  const act = useCallback(async (work: () => Promise<BotView>): Promise<void> => {
    try {
      const view = await work();
      setBots((listed) =>
        listed === null ? listed : listed.map((bot) => (bot.botId === view.botId ? view : bot)),
      );
      setError(null);
    } catch (cause: unknown) {
      setError(friendlyError(cause));
    }
  }, []);

  return (
    <div className="panel bots-panel">
      <h1 className="panel-title">Bots</h1>
      <p className="games-lede">
        A bot is an account you run. It signs in with a token, speaks as itself in the conversations
        it joins, and holds exactly the permissions you give it — which start at none.
      </p>

      {error !== null ? <p className="form-error">{error}</p> : null}
      {notice !== null ? <p className="bots-notice">{notice}</p> : null}

      {/* The one-time reveal. It sits above the list on purpose: a token that has to be scrolled
          to is a token somebody rotates again for no reason. */}
      {reveal !== null ? (
        <div className="bots-reveal">
          <p className="bots-reveal-title">
            {reveal.rotated ? 'New token for ' : 'Token for '}
            {reveal.name}
          </p>
          <p className="bots-reveal-hint">
            This is the only time it is shown. The server stores a tag of it, not the token, so
            nobody — including this app — can print it again. Put it somewhere safe now; if you lose
            it, rotate to mint another.
          </p>
          <code className="bots-token">{reveal.token}</code>
          <div className="bots-reveal-actions">
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => {
                void navigator.clipboard.writeText(reveal.token).catch(() => {});
                setNotice('Token copied to the clipboard.');
              }}
            >
              Copy token
            </button>
            <button type="button" className="btn btn-ghost" onClick={() => setReveal(null)}>
              I have saved it
            </button>
          </div>
        </div>
      ) : null}

      <RegisterForm
        onRegistered={(view) => {
          setBots((listed) => (listed === null ? [view] : [...listed, view]));
          setError(null);
          setReveal(
            view.token === undefined
              ? null
              : {
                  botId: view.botId,
                  name: view.name,
                  token: view.token,
                  rotated: false,
                },
          );
          if (view.token === undefined) {
            // A node that answered without a token has nothing to show, so say what happened
            // rather than leaving an empty list row as the only evidence.
            setNotice(`${view.name} was created, but this server did not return a token.`);
          }
        }}
      />

      {events.length > 0 ? (
        <div className="bots-events">
          {events.map((event, index) => (
            <p className="bots-event" key={`${event.botId}-${event.event}-${index}`}>
              <Icon name="info" size={14} /> {nameOf(bots, event.botId)}: {event.event}
            </p>
          ))}
        </div>
      ) : null}

      {bots === null && error === null ? <Skeleton rows={2} /> : null}

      {bots !== null && bots.length === 0 ? (
        <EmptyState
          icon="bot"
          title="No bots yet"
          hint="Register one above and give it the permissions it needs — nothing more."
        />
      ) : null}

      {bots !== null && bots.length > 0 ? (
        <div className="bots-list">
          {bots.map((bot) => (
            <div className="bot-card" key={bot.botId}>
              <div className="bot-card-head">
                <span className="bot-card-icon" aria-hidden="true">
                  <Icon name="bot" size={20} />
                </span>
                <span className="bot-card-name">{bot.name}</span>
                <span className={bot.paused === true ? 'bot-chip bot-chip-off' : 'bot-chip'}>
                  {bot.paused === true ? 'Paused' : 'Active'}
                </span>
              </div>

              <p className="bot-card-scopes">
                {bot.scopes === undefined
                  ? 'This server did not say what this bot may do.'
                  : bot.scopes.length === 0
                    ? 'No permissions.'
                    : bot.scopes.map((scope) => scopeLabel(scope)).join(' · ')}
              </p>

              <div className="bot-card-actions">
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={() => {
                    if (client) {
                      void act(() => client.bots.setPaused(bot.botId, bot.paused !== true));
                    }
                  }}
                >
                  {bot.paused === true ? 'Resume' : 'Pause'}
                </button>
                <button
                  type="button"
                  className="btn btn-ghost"
                  onClick={() => {
                    setConfirming(null);
                    setEditing(editing === bot.botId ? null : bot.botId);
                  }}
                >
                  Permissions
                </button>

                {/* Rotation is destructive to the credential in use right now, so it takes two
                    clicks: the first asks, the second does. A single click here would break a
                    running bot on a mis-tap. */}
                {confirming === bot.botId ? (
                  <button
                    type="button"
                    className="btn btn-danger"
                    onClick={() => {
                      setConfirming(null);
                      if (!client) {
                        return;
                      }
                      void (async () => {
                        try {
                          const view = await client.bots.rotate(bot.botId);
                          setBots((listed) =>
                            listed === null
                              ? listed
                              : listed.map((row) => (row.botId === view.botId ? view : row)),
                          );
                          setError(null);
                          if (view.token !== undefined) {
                            setReveal({
                              botId: view.botId,
                              name: view.name,
                              token: view.token,
                              rotated: true,
                            });
                          }
                        } catch (cause: unknown) {
                          setError(friendlyError(cause));
                        }
                      })();
                    }}
                  >
                    Rotate now — the old token stops working
                  </button>
                ) : (
                  <button
                    type="button"
                    className="btn btn-ghost"
                    onClick={() => setConfirming(bot.botId)}
                  >
                    New token
                  </button>
                )}
              </div>

              {editing === bot.botId ? (
                <ScopeEditor
                  held={bot.scopes}
                  onCancel={() => setEditing(null)}
                  onSave={(scopes) => {
                    setEditing(null);
                    if (client) {
                      void act(() => client.bots.setScopes(bot.botId, scopes));
                    }
                  }}
                />
              ) : null}
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** The register form: a handle and a display name, and nothing else. */
function RegisterForm({ onRegistered }: { onRegistered: (view: BotView) => void }): ReactNode {
  const { client } = useMigo();
  const [username, setUsername] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent): Promise<void> {
    event.preventDefault();
    if (!client || busy) {
      return;
    }
    setBusy(true);
    try {
      const view = await client.bots.register(username.trim(), displayName.trim());
      setUsername('');
      setDisplayName('');
      setError(null);
      onRegistered(view);
    } catch (cause: unknown) {
      setError(friendlyError(cause));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="bots-register" onSubmit={(event) => void submit(event)}>
      <p className="bots-register-title">Register a bot</p>
      <div className="bots-register-fields">
        <label className="field-label">
          Handle
          <input
            value={username}
            onChange={(event) => setUsername(event.target.value)}
            placeholder="weather"
            autoComplete="off"
            spellCheck={false}
            required
          />
        </label>
        <label className="field-label">
          Display name
          <input
            value={displayName}
            onChange={(event) => setDisplayName(event.target.value)}
            placeholder="Weather"
            autoComplete="off"
            required
          />
        </label>
      </div>
      <p className="field-hint">
        The handle is the account it signs in with — lowercase letters, digits, dots and
        underscores, and it has to be free. The display name is what people see beside its messages.
      </p>
      {error !== null ? <p className="form-error">{error}</p> : null}
      <button type="submit" className="btn btn-primary" disabled={busy}>
        {busy ? 'Creating…' : 'Create bot'}
      </button>
    </form>
  );
}

/** The permission picker: a whole set, saved whole. */
function ScopeEditor({
  held,
  onSave,
  onCancel,
}: {
  /** What the node said the bot holds; `undefined` means the server did not say. */
  held: readonly string[] | undefined;
  onSave: (scopes: readonly BotScope[]) => void;
  onCancel: () => void;
}): ReactNode {
  const [chosen, setChosen] = useState<ReadonlySet<string>>(
    () => new Set(held === undefined ? NO_SCOPES : held),
  );

  function toggle(slug: BotScope): void {
    setChosen((current) => {
      const next = new Set(current);
      if (next.has(slug)) {
        next.delete(slug);
      } else {
        next.add(slug);
      }
      return next;
    });
  }

  return (
    <div className="bot-scopes">
      {held === undefined ? (
        <p className="field-hint">
          This server did not report what the bot holds, so nothing is ticked. Saving replaces
          whatever it holds with exactly what is ticked here.
        </p>
      ) : null}
      {BOT_SCOPES.map((slug) => (
        <label className="bot-scope" key={slug}>
          <input type="checkbox" checked={chosen.has(slug)} onChange={() => toggle(slug)} />
          <span>
            <span className="bot-scope-label">{SCOPE_LABEL[slug]}</span>
            <span className="bot-scope-hint">{SCOPE_HINT[slug]}</span>
          </span>
        </label>
      ))}
      <div className="bot-card-actions">
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => onSave(BOT_SCOPES.filter((slug) => chosen.has(slug)))}
        >
          Save permissions
        </button>
        <button type="button" className="btn btn-ghost" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}

/** The label for a slug, falling back to the slug itself for one this build does not name. */
function scopeLabel(slug: string): string {
  return (SCOPE_LABEL as Readonly<Record<string, string>>)[slug] ?? slug;
}

/** A bot's display name from the list in hand, or its id when the list does not carry it. */
function nameOf(bots: BotView[] | null, botId: Id): string {
  const found = bots?.find((bot) => bot.botId === botId);
  return found === undefined ? 'A bot' : found.name;
}
