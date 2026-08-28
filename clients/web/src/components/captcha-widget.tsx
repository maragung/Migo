'use client';

/**
 * The captcha widget the auth forms show once the network is over the gate.
 *
 * The bootstrap surface is captcha-gated: after a few failed sign-in attempts
 * from the same network, the server starts refusing register and login bodies
 * that do not carry a {@link CaptchaProof}. The proof has to be fresh — the
 * server consumes the challenge on use — so the widget fetches one on mount
 * and exposes the proof to the parent form once the user has typed an answer.
 *
 * The widget has no idea which endpoint it lives on: it takes the {@link
 * ServerEndpoint} the user picked (or the default) and builds a one-shot
 * {@link BootstrapClient} over it, so the captcha is requested from the same
 * server the form will hand credentials to. The form is the source of truth for
 * which server is "this server" — the widget just rides on the form's choice.
 *
 * # Why the widget manages its own request
 *
 * Putting the captcha fetch in the form's parent would make the captcha a
 * second concern every page that signs in has to know about: it would have to
 * coordinate the request, surface the question, and pass the proof down. The
 * widget is a self-contained input that yields a {@link CaptchaProof} on demand
 * and a `null` while it is still loading or after the user clears the field.
 * A regression that broke the wire shape (a renamed field, a wrong id type) is
 * caught by the integration test that mounts the register page and asserts the
 * body the form posts.
 */

import { useEffect, useId, useState } from 'react';
import type { ReactNode } from 'react';

import { parseId } from '@migo/wire';
import type { Id } from '@migo/wire';

import { BootstrapClient, RemoteError } from '@migo/sdk';
import type { CaptchaProof, ServerEndpoint } from '@migo/sdk';

export interface CaptchaWidgetProps {
  /** The server the form is about to authenticate against. */
  endpoint: ServerEndpoint;
  /**
   * Called when the user has typed a six-digit answer and the proof is ready
   * to attach to the next register/login attempt. Called with `null` while
   * the challenge is still loading, the answer is empty, or the refresh
   * failed — a `null` means the form should not submit a proof.
   */
  onChange: (proof: CaptchaProof | null) => void;
}

interface WidgetState {
  challengeId: Id | null;
  question: string | null;
  answer: string;
  status: 'loading' | 'ready' | 'error';
  errorMessage: string | null;
}

const EMPTY: WidgetState = {
  challengeId: null,
  question: null,
  answer: '',
  status: 'loading',
  errorMessage: null,
};

/**
 * Mounts a captcha question, an answer field, and a refresh button. The form
 * reads the proof through `onChange`; the widget never calls the network on
 * the user's behalf except for the one request that loads a fresh challenge.
 */
export function CaptchaWidget({ endpoint, onChange }: CaptchaWidgetProps): ReactNode {
  const [state, setState] = useState<WidgetState>(EMPTY);
  const questionId = useId();
  const answerId = useId();

  // One captcha per (endpoint, mount) pair. The endpoint is part of the key
  // because a self-hosted user who flips the disclosure to a different host
  // needs a challenge the new server actually minted, not a stale one from
  // the previous host that the new server would refuse to verify.
  useEffect(() => {
    let cancelled = false;
    const bootstrap = new BootstrapClient(endpoint);
    setState(EMPTY);
    bootstrap
      .requestCaptcha()
      .then((challenge) => {
        if (cancelled) return;
        setState({
          // `challenge.challenge_id` is already the SDK's parsed `Id`; we re-parse so a server
          // that hands us a slightly wrong shape (extra whitespace, a lowercase variant) does
          // not slip an unparseable string into the body and turn the form into an oracle of
          // captcha id parsing rules.
          challengeId: parseId(String(challenge.challenge_id)),
          question: challenge.question,
          answer: '',
          status: 'ready',
          errorMessage: null,
        });
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setState({
          challengeId: null,
          question: null,
          answer: '',
          status: 'error',
          errorMessage: messageFor(cause),
        });
      });
    return () => {
      cancelled = true;
    };
  }, [endpoint]);

  // Notify the parent whenever the proof is complete and well-formed, or
  // whenever the user has invalidated it. The proof is exactly what the
  // bootstrap call will copy into the body: the `challenge_id` the server
  // minted and the user's typed `answer`, both as plain strings.
  useEffect(() => {
    if (state.status !== 'ready' || state.challengeId === null) {
      onChange(null);
      return;
    }
    if (!/^\d{6}$/.test(state.answer)) {
      onChange(null);
      return;
    }
    onChange({ challenge_id: state.challengeId, answer: state.answer });
  }, [state, onChange]);

  const refresh = (): void => {
    setState(EMPTY);
    const bootstrap = new BootstrapClient(endpoint);
    bootstrap
      .requestCaptcha()
      .then((challenge) => {
        setState({
          challengeId: parseId(String(challenge.challenge_id)),
          question: challenge.question,
          answer: '',
          status: 'ready',
          errorMessage: null,
        });
      })
      .catch((cause: unknown) => {
        setState({
          challengeId: null,
          question: null,
          answer: '',
          status: 'error',
          errorMessage: messageFor(cause),
        });
      });
  };

  return (
    <section className="captcha-widget" aria-busy={state.status === 'loading'}>
      <div className="captcha-row">
        <label className="field-label" htmlFor={questionId}>
          Captcha
          <output id={questionId} className="captcha-question" aria-live="polite">
            {state.status === 'loading' ? 'Loading challenge…' : (state.question ?? '—')}
          </output>
        </label>
        <button
          type="button"
          className="btn btn-secondary captcha-refresh"
          onClick={refresh}
          disabled={state.status === 'loading'}
          aria-label="Request a new captcha"
        >
          ↻
        </button>
      </div>
      <label className="field-label" htmlFor={answerId}>
        Answer
        <input
          id={answerId}
          type="text"
          inputMode="numeric"
          autoComplete="off"
          pattern="\d{6}"
          maxLength={6}
          placeholder="Six digits"
          value={state.answer}
          onChange={(event) =>
            setState((current) => ({ ...current, answer: event.target.value.replace(/\D/g, '') }))
          }
        />
      </label>
      {state.errorMessage !== null ? <p className="form-error">{state.errorMessage}</p> : null}
    </section>
  );
}

/** A short, user-readable message for a captcha-request failure. */
function messageFor(cause: unknown): string {
  if (cause instanceof RemoteError) {
    return 'Could not load a captcha challenge. Refresh and try again.';
  }
  return 'Could not reach the server. Check your connection and try again.';
}
