'use client';

/**
 * The "Games" control in a conversation's header: a button that opens the node's game catalogue
 * and starts one in this conversation.
 *
 * The catalogue is fetched when the popover first opens, not when the thread mounts — a user who
 * never touches the button should never pay for its data — and is kept for the thread's life, the
 * same session-scoped posture the panel data uses.
 *
 * Only single-player games are startable through this build's wire: `GAME_START` cannot name
 * opponents, so the server refuses a two-player kind outright. Rather than send a request the
 * protocol has already doomed, the entry renders disabled with the reason beside it — the button
 * says why it does nothing instead of failing after a round trip.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';

import type { GameCatalogueEntry } from '@migo/sdk';

import { gameLabelOf, playerRangeLabel } from '@/lib/games.js';
import { friendlyError } from '@/lib/migo/errors.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Spinner } from './spinner.js';

export function GameLauncher({
  onStart,
}: {
  /** Starts a game by catalogue slug; rejects when the server refuses. */
  onStart: (slug: string) => Promise<unknown>;
}): ReactNode {
  const { client } = useMigo();

  const [open, setOpen] = useState(false);
  const [catalogue, setCatalogue] = useState<GameCatalogueEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState<string | null>(null);

  // The catalogue loads on first open and stays for the thread's life; a failure is retried by
  // closing and reopening, which is cheaper to discover than a button that does nothing.
  useEffect(() => {
    if (!open || catalogue !== null || !client) {
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
  }, [open, catalogue, client]);

  // Close on Escape: the popover is a transient menu, and keyboard users should be able to
  // dismiss it the same way they dismiss a dialog, without tabbing to a close target.
  useEffect(() => {
    if (!open) {
      return;
    }
    function onKey(event: KeyboardEvent): void {
      if (event.key === 'Escape') {
        setOpen(false);
      }
    }
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const start = useCallback(
    (entry: GameCatalogueEntry): void => {
      if (starting !== null || entry.minPlayers > 1) {
        return;
      }
      setStarting(entry.slug);
      setError(null);
      onStart(entry.slug)
        .then(() => {
          setOpen(false);
        })
        .catch((cause: unknown) => {
          setError(friendlyError(cause));
        })
        .finally(() => {
          setStarting(null);
        });
    },
    [onStart, starting],
  );

  return (
    <div className="game-launcher">
      <button
        type="button"
        className="btn btn-ghost games-btn"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        🎮 <span className="games-label">Games</span>
      </button>
      {open ? (
        <>
          {/* A click on the page outside the menu closes it; the transparent layer is the whole
              rest of the viewport, so the affordance does not depend on hitting a small target. */}
          <button
            type="button"
            className="menu-backdrop"
            aria-label="Close game menu"
            onClick={() => setOpen(false)}
          />
          <div className="game-menu" role="menu" aria-label="Start a game">
            {catalogue === null ? (
              <div className="game-menu-status">
                {error !== null ? <span className="form-error">{error}</span> : <Spinner />}
              </div>
            ) : catalogue.length === 0 ? (
              <div className="game-menu-status">
                <span className="muted">No games on this server.</span>
              </div>
            ) : (
              catalogue.map((entry) => {
                const solo = entry.minPlayers <= 1;
                return (
                  <button
                    key={entry.slug}
                    type="button"
                    role="menuitem"
                    className="game-menu-item"
                    disabled={!solo || starting !== null}
                    title={
                      solo
                        ? `Start ${gameLabelOf(entry.kind)}`
                        : 'Needs two players, and this build cannot pick an opponent'
                    }
                    onClick={() => start(entry)}
                  >
                    <span className="game-menu-name">{gameLabelOf(entry.kind)}</span>
                    <span className="game-menu-note">
                      {starting === entry.slug ? (
                        <Spinner />
                      ) : (
                        playerRangeLabel(entry.minPlayers, entry.maxPlayers)
                      )}
                    </span>
                    {!solo ? (
                      <span className="game-menu-note">needs an opponent picker</span>
                    ) : null}
                  </button>
                );
              })
            )}
            {catalogue !== null && error !== null ? (
              <div className="game-menu-status">
                <span className="form-error">{error}</span>
              </div>
            ) : null}
          </div>
        </>
      ) : null}
    </div>
  );
}
