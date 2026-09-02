'use client';

/**
 * The Games tab: the game catalogue as a top-level destination.
 *
 * The reference's Games tab is an arcade with a dice table; this build's wire honestly offers
 * something narrower — a catalogue of games that are started *inside a conversation* (see the
 * thread header's {@link GameLauncher}), of which only the single-player kinds can be started
 * at all, because `GAME_START` cannot name opponents. The panel therefore leads with the
 * catalogue the server publishes — name, player range, the slug the launcher starts — and
 * answers "play" by opening the new-conversation dialog, whose completion opens the thread as
 * a chat tab where a game can actually begin. Nothing here invents an opponent or a stake the
 * server does not carry.
 *
 * `onActivate` is the left panel's ask: a list that carries it turns its cards into doors that
 * open the arcade as the right pane's Games tab — the click on the left is what the right pane
 * shows. The pane's own instance passes nothing; its cards state the catalogue, the button
 * below them starts the flow.
 */

import { useEffect, useState } from 'react';
import type { KeyboardEvent, ReactNode } from 'react';

import type { GameCatalogueEntry } from '@migo/sdk';

import { gameLabelOf, playerRangeLabel } from '@/lib/games.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { NewConversationDialog } from './new-conversation-dialog.js';
import { EmptyState } from './states.js';
import { Skeleton } from './states.js';

/** Keyboard support for the clickable cards: Enter or Space activates, matching a button. */
function activateOnEnter(onActivate: () => void): (event: KeyboardEvent<HTMLDivElement>) => void {
  return (event) => {
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault();
      onActivate();
    }
  };
}

/** The catalogue panel. */
export function GamesPanel({ onActivate }: { onActivate?: () => void }): ReactNode {
  const { client } = useMigo();
  const [catalogue, setCatalogue] = useState<GameCatalogueEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);

  // One read per mount; the catalogue is server-owned fact and the thread's launcher re-reads
  // its own copy when a game is actually started.
  useEffect(() => {
    if (!client) {
      return;
    }
    let cancelled = false;
    client.games
      .getCatalogue()
      .then((games) => {
        if (!cancelled) {
          setCatalogue(games);
          setError(null);
        }
      })
      .catch((cause: unknown) => {
        if (!cancelled) {
          setError(friendlyError(cause));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  return (
    <div className="panel games-panel">
      <h1 className="panel-title">Games</h1>
      <p className="games-lede">
        Games start inside a conversation — pick one from the catalogue and open a chat to play.
      </p>

      {error !== null ? <p className="form-error">{error}</p> : null}

      {catalogue === null && error === null ? <Skeleton rows={3} /> : null}

      {catalogue !== null && catalogue.length === 0 ? (
        <EmptyState
          icon="game"
          title="No games on this server"
          hint="The catalogue is empty; the server decides what is offered."
        />
      ) : null}

      {catalogue !== null && catalogue.length > 0 ? (
        <div className="games-grid">
          {catalogue.map((entry) => (
            <div
              key={entry.slug}
              className="game-card"
              {...(onActivate !== undefined
                ? {
                    role: 'button',
                    tabIndex: 0,
                    onClick: onActivate,
                    onKeyDown: activateOnEnter(onActivate),
                  }
                : {})}
            >
              <span className="game-card-icon" aria-hidden="true">
                🎮
              </span>
              <span className="game-card-name">{gameLabelOf(entry.kind)}</span>
              <span className="game-card-range">
                {playerRangeLabel(entry.minPlayers, entry.maxPlayers)}
              </span>
            </div>
          ))}
        </div>
      ) : null}

      <button type="button" className="btn btn-primary" onClick={() => setDialogOpen(true)}>
        Open a chat to play
      </button>

      {dialogOpen ? <NewConversationDialog onClose={() => setDialogOpen(false)} /> : null}
    </div>
  );
}
