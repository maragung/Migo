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
 * A challenge is a picture, not a question in the clear: the server hands back
 * a PNG the user reads five to six letters and digits off, and nothing about
 * the answer crosses the wire until the user has typed it. Because a distorted
 * image is exactly what some users cannot read, the widget also carries an
 * "Easier challenge" control: it asks the server for `image_alt`, a
 * freshly-issued challenge (a different random code) rendered with larger
 * glyphs and less noise. It is still an image to solve — the product decision
 * is a gentler picture, not a different puzzle — so the answer field and the
 * proof are identical for both modes.
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
 * coordinate the request, render the image, and pass the proof down. The
 * widget is a self-contained input that yields a {@link CaptchaProof} on demand
 * and a `null` while it is still loading or after the user clears the field.
 * A regression that broke the wire shape (a renamed field, a wrong id type) is
 * caught by the integration test that mounts the register page and asserts the
 * body the form posts.
 *
 * # What the widget announces, and to whom
 *
 * The image carries an alt description of the *task*, never the answer — the
 * answer exists only in the user's typing and must not leak into the DOM, the
 * markup, or a log. The polite live region below the image announces challenge
 * lifecycle only ("loading", "loaded"); that is the one thing a screen-reader
 * user needs to hear that the picture cannot tell them.
 */

import { useEffect, useId, useState } from 'react';
import type { ReactNode } from 'react';

import { parseId } from '@migo/wire';
import type { Id } from '@migo/wire';

import { BootstrapClient, RemoteError } from '@migo/sdk';
import type { CaptchaChallenge, CaptchaMode, CaptchaProof, ServerEndpoint } from '@migo/sdk';

export interface CaptchaWidgetProps {
  /** The server the form is about to authenticate against. */
  endpoint: ServerEndpoint;
  /**
   * Called when the user has typed an answer read off the challenge image and
   * the proof is ready to attach to the next register/login attempt. Called
   * with `null` while the challenge is still loading, while the answer is
   * empty or too short, or after a refresh failed — a `null` means the form
   * should not submit a proof.
   */
  onChange: (proof: CaptchaProof | null) => void;
}

interface WidgetState {
  challengeId: Id | null;
  imagePngBase64: string | null;
  mode: CaptchaMode;
  answer: string;
  status: 'loading' | 'ready' | 'error';
  errorMessage: string | null;
}

const EMPTY: WidgetState = {
  challengeId: null,
  imagePngBase64: null,
  mode: 'image',
  answer: '',
  status: 'loading',
  errorMessage: null,
};

/**
 * A blank, transparent SVG the `<img>` carries until the real challenge PNG arrives.
 *
 * Mounting the image element from the first paint — rather than only once a challenge
 * is in hand — reserves the layout box, keeps the alt description in the markup a
 * static render sees, and lets a refresh swap the picture in place instead of popping
 * a new element into the form. The CSS background and border turn the blank into a
 * visible "loading" panel that reads on both the light and dark themes.
 */
const PLACEHOLDER_SRC =
  "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='200' height='64'%3E%3C/svg%3E";

/**
 * Strips every whitespace character and upper-cases the rest, which is exactly the
 * normalisation the server applies before comparing. Applying it before the proof
 * leaves the client means the field can be forgiving about the spaces and the case a
 * user types, while the form receives the answer in the one shape the server expects.
 * The normalised answer lives only in this component's state and the proof it hands
 * up; it is never written anywhere else.
 */
function normalizeAnswer(raw: string): string {
  return raw.replace(/\s+/g, '').toUpperCase();
}

/** Maps a fetched challenge into the widget's ready state. */
function loadedFrom(challenge: CaptchaChallenge): WidgetState {
  return {
    // `challenge.challenge_id` is already the SDK's parsed `Id`; we re-parse so a server
    // that hands us a slightly wrong shape (extra whitespace, a lowercase variant) does
    // not slip an unparseable string into the body and turn the form into an oracle of
    // captcha id parsing rules.
    challengeId: parseId(String(challenge.challenge_id)),
    imagePngBase64: challenge.image_png_base64,
    // The server echoes the mode it actually issued, so refresh asks for the same
    // rendering again: a user who switched to the easier challenge keeps it.
    mode: challenge.mode,
    answer: '',
    status: 'ready',
    errorMessage: null,
  };
}

/**
 * Maps a failed challenge request into the widget's error state, keeping the mode the
 * user asked for so a refresh retries the rendering they wanted.
 */
function failedFrom(mode: CaptchaMode, cause: unknown): WidgetState {
  return { ...EMPTY, mode, status: 'error', errorMessage: messageFor(cause) };
}

/**
 * Mounts a challenge image, an answer field, a refresh button, and an easier-challenge
 * button. The form reads the proof through `onChange`; the widget never calls the
 * network on the user's behalf except for the requests that load a fresh challenge.
 */
export function CaptchaWidget({ endpoint, onChange }: CaptchaWidgetProps): ReactNode {
  const [state, setState] = useState<WidgetState>(EMPTY);
  const answerId = useId();

  // One captcha per (endpoint, mount) pair. The endpoint is part of the key
  // because a self-hosted user who picks a different host in the server sheet
  // needs a challenge the new server actually minted, not a stale one from
  // the previous host that the new server would refuse to verify. A new
  // server also means a fresh start from the default rendering: the alt-mode
  // preference belongs to the challenge session, not to the endpoint.
  useEffect(() => {
    let cancelled = false;
    const bootstrap = new BootstrapClient(endpoint);
    setState(EMPTY);
    bootstrap
      .requestCaptcha()
      .then((challenge) => {
        if (cancelled) return;
        setState(loadedFrom(challenge));
      })
      .catch((cause: unknown) => {
        if (cancelled) return;
        setState(failedFrom('image', cause));
      });
    return () => {
      cancelled = true;
    };
  }, [endpoint]);

  // Notify the parent whenever the proof is complete and well-formed, or
  // whenever the user has invalidated it. The proof is exactly what the
  // bootstrap call will copy into the body: the `challenge_id` the server
  // minted and the user's typed `answer`, both as plain strings. The answer
  // is already normalised by the input handler, so the shape check runs on
  // precisely what would be sent.
  useEffect(() => {
    if (state.status !== 'ready' || state.challengeId === null) {
      onChange(null);
      return;
    }
    if (!/^[A-Za-z0-9]{5,6}$/.test(state.answer)) {
      onChange(null);
      return;
    }
    onChange({ challenge_id: state.challengeId, answer: state.answer });
  }, [state, onChange]);

  // Refresh keeps the mode the current challenge was issued in (a user who
  // asked for the easier rendering keeps it); the easier-challenge button
  // deliberately moves to the alt rendering, which the server issues as a
  // fresh challenge with a different code. Both share this one path: reset
  // to loading, fetch, and land in ready or the shared error state.
  const load = (nextMode: CaptchaMode): void => {
    setState({ ...EMPTY, mode: nextMode });
    const bootstrap = new BootstrapClient(endpoint);
    bootstrap
      .requestCaptcha(nextMode)
      .then((challenge) => {
        setState(loadedFrom(challenge));
      })
      .catch((cause: unknown) => {
        setState(failedFrom(nextMode, cause));
      });
  };

  const refresh = (): void => {
    load(state.mode);
  };

  const imageSrc =
    state.imagePngBase64 === null
      ? PLACEHOLDER_SRC
      : `data:image/png;base64,${state.imagePngBase64}`;

  return (
    <section className="captcha-widget" aria-busy={state.status === 'loading'}>
      <div className="captcha-row">
        <span className="field-label">Captcha</span>
        <div className="captcha-actions">
          <button
            type="button"
            className="btn btn-secondary captcha-refresh"
            onClick={refresh}
            disabled={state.status === 'loading'}
            aria-label="Request a new captcha"
          >
            ↻
          </button>
          <button
            type="button"
            className="btn btn-secondary captcha-alt"
            onClick={() => load('image_alt')}
            disabled={state.status === 'loading'}
            aria-label="Request an easier-to-read captcha"
            title="Ask for a fresh challenge with larger, clearer characters"
          >
            Easier challenge
          </button>
        </div>
      </div>
      <img
        className="captcha-image"
        src={imageSrc}
        alt="Captcha: five to six letters and digits, distorted"
      />
      <p className="captcha-status" aria-live="polite">
        {state.status === 'loading'
          ? 'Loading challenge…'
          : state.status === 'ready'
            ? 'New challenge loaded.'
            : ''}
      </p>
      <label className="field-label" htmlFor={answerId}>
        Answer
        <input
          id={answerId}
          type="text"
          inputMode="text"
          autoCapitalize="characters"
          autoComplete="off"
          spellCheck={false}
          pattern="[A-Za-z0-9]{5,6}"
          maxLength={6}
          placeholder="5–6 characters"
          value={state.answer}
          onChange={(event) =>
            setState((current) => ({ ...current, answer: normalizeAnswer(event.target.value) }))
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
