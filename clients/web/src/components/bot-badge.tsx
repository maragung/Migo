import type { Id } from '@migo/sdk';
import type { ReactNode } from 'react';

/**
 * The mark that says an account is a bot.
 *
 * Brief section 49's last open sentence is that a bot could be reported and never seen: a bot holds
 * an ordinary account row and an ordinary profile, so every listing in this client drew one as a
 * person. The wire now names the bot behind such an account — `bot_id` on a profile card and on a
 * search result — and this is the one place that turns the name into something a reader can see.
 *
 * It renders nothing when there is no bot, rather than an empty span, because the mark has to be
 * absent often enough to mean something when it is here. It answers only the question "is this a
 * bot"; the id itself belongs to whoever reports one, and that caller reads it off the same field.
 */
export function BotBadge({
  botId,
  compact = false,
}: {
  /** The bot this account speaks as; nothing is drawn when the account is not one. */
  botId?: Id;
  /** Drops the word, for a dense row: the glyph and its tooltip alone. */
  compact?: boolean;
}): ReactNode {
  if (botId === undefined) {
    return null;
  }
  return (
    <span
      className={compact ? 'bot-badge bot-badge-compact' : 'bot-badge'}
      title="This is a bot, not a person: a program its owner runs. Report it as a bot if it misbehaves."
    >
      <span aria-hidden="true">🤖</span>
      {compact ? <span className="visually-hidden">Bot</span> : <span>Bot</span>}
    </span>
  );
}
