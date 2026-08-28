'use client';

/**
 * The "Server" disclosure on the sign-in and registration forms.
 *
 * A user who has never opened this disclosure sees exactly the form they saw yesterday: identifier
 * and password. A user who has opened it picks the host, port, transport and scheme, and on
 * "Use this server" the disclosure closes and the choice becomes the new form input.
 *
 * The QUIC choice is a menu item that does not yet have a wired path; selecting it shows
 * "coming soon" inline and never blocks submit. The form still accepts the choice, and the rest
 * of the surface (REST, captcha) proceeds, so the user can finish setting up a server and try
 * QUIC the day it actually lands.
 */

import { useId, useState } from 'react';
import type { ReactNode } from 'react';

import { defaultSchemesForHost, isLoopbackHost, parseHost, validatePorts } from '@migo/sdk';
import type { RestScheme, Scheme, ServerEndpoint, Transport, WsScheme } from '@migo/sdk';

export interface ServerFormProps {
  /** The initial value, e.g. the persisted endpoint or the env default. */
  value: ServerEndpoint;
  /**
   * Called when the user has picked an endpoint and confirmed it. The form does not call this on
   * every keystroke -- the host and port fields are local until "Use this server" is clicked, so
   * the parent form is not revalidated while the user is still typing.
   */
  onCommit: (next: ServerEndpoint) => void;
}

/** The intermediate, user-typed form values, before they are validated. */
interface FormState {
  host: string;
  port: string;
  gatewayPort: string;
  transport: Transport;
  scheme: Scheme;
  restScheme: RestScheme;
}

function toForm(endpoint: ServerEndpoint): FormState {
  return {
    host: endpoint.host,
    port: String(endpoint.port),
    gatewayPort: String(endpoint.gatewayPort),
    transport: endpoint.transport,
    scheme: endpoint.scheme,
    restScheme: endpoint.restScheme,
  };
}

/**
 * Renders the disclosure and the inline confirm button. The disclosure's `open` state is local
 * to this component; the parent's `value` is the only thing the parent cares about, and that
 * only updates when the user clicks "Use this server".
 */
export function ServerForm({ value, onCommit }: ServerFormProps): ReactNode {
  const [open, setOpen] = useState(false);
  const [draft, setDraft] = useState<FormState>(toForm(value));
  const [error, setError] = useState<string | null>(null);
  const summaryId = useId();
  const hostId = useId();
  const portId = useId();
  const transportId = useId();
  const schemeId = useId();

  const updateHost = (host: string): void => {
    const trimmed = host.trim();
    setDraft((current) => {
      const next: FormState = { ...current, host: trimmed };
      if (current.transport === 'WebSocket') {
        // Pairs the scheme with the loopback rule on the fly, so the user never sees a "WSS for
        // localhost" placeholder they did not choose.
        next.scheme = defaultSchemesForHost(trimmed).scheme;
        next.restScheme = defaultSchemesForHost(trimmed).restScheme;
      }
      return next;
    });
  };

  const updateTransport = (transport: Transport): void => {
    setDraft((current) => {
      if (transport === 'Quic') {
        return {
          ...current,
          transport,
          scheme: isLoopbackHost(current.host) ? 'Quic' : 'QuicTls',
          restScheme: isLoopbackHost(current.host) ? 'Http' : 'Https',
        };
      }
      return { ...current, transport, ...defaultSchemesForTransport('WebSocket', current.host) };
    });
  };

  const commit = (): void => {
    try {
      const next = buildFromForm(draft);
      onCommit(next);
      setOpen(false);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'invalid server');
    }
  };

  const summary = `${draft.host || 'unset'}:${draft.port || '?'}`;
  return (
    <section className="server-disclosure" aria-labelledby={summaryId}>
      <button
        type="button"
        className="server-disclosure-toggle"
        aria-expanded={open}
        aria-controls={`${summaryId}-panel`}
        onClick={() => setOpen((current) => !current)}
      >
        <span className="server-disclosure-icon" aria-hidden="true">
          {open ? '▾' : '▸'}
        </span>
        <span id={summaryId}>Server</span>
        {!open ? (
          <span className="server-disclosure-summary" aria-live="polite">
            {summary}
          </span>
        ) : null}
      </button>
      {open ? (
        <div id={`${summaryId}-panel`} className="server-disclosure-panel">
          <div className="server-disclosure-row">
            <label className="field-label" htmlFor={hostId}>
              Host
              <input
                id={hostId}
                type="text"
                inputMode="url"
                autoComplete="off"
                placeholder="migo.example.com"
                value={draft.host}
                onChange={(event) => updateHost(event.target.value)}
              />
            </label>
            <label className="field-label" htmlFor={portId}>
              Port
              <input
                id={portId}
                type="number"
                min={1}
                max={65535}
                placeholder="18080"
                value={draft.port}
                onChange={(event) =>
                  setDraft((current) => ({ ...current, port: event.target.value }))
                }
              />
            </label>
          </div>
          <div className="server-disclosure-row">
            <label className="field-label" htmlFor={transportId}>
              Transport
              <select
                id={transportId}
                value={draft.transport}
                onChange={(event) => updateTransport(event.target.value as Transport)}
              >
                <option value="WebSocket">WebSocket</option>
                <option value="Quic">QUIC</option>
              </select>
            </label>
            <label className="field-label" htmlFor={schemeId}>
              Scheme
              <select
                id={schemeId}
                value={draft.scheme}
                onChange={(event) => {
                  const scheme = event.target.value as Scheme;
                  setDraft((current) => ({
                    ...current,
                    scheme,
                    restScheme: schemeToRestScheme(scheme, current.transport),
                  }));
                }}
              >
                {draft.transport === 'WebSocket' ? (
                  <>
                    <option value="Ws">WS (plain, dev-only)</option>
                    <option value="Wss">WSS (TLS)</option>
                  </>
                ) : (
                  <>
                    <option value="Quic">QUIC (plain)</option>
                    <option value="QuicTls">QUIC-TLS</option>
                  </>
                )}
              </select>
            </label>
          </div>
          {draft.transport === 'Quic' ? (
            <p className="server-disclosure-note" role="status">
              QUIC support is coming soon. The form will not block; pick WebSocket to sign in today.
            </p>
          ) : null}
          {error ? <p className="form-error">{error}</p> : null}
          <div className="server-disclosure-actions">
            <button type="button" className="btn btn-secondary" onClick={commit}>
              Use this server
            </button>
          </div>
        </div>
      ) : null}
    </section>
  );
}

/** Picks a REST scheme that pairs with a transport scheme, the form's one explicit coupling. */
function schemeToRestScheme(scheme: Scheme, transport: Transport): RestScheme {
  if (transport === 'Quic') {
    return scheme === 'QuicTls' ? 'Https' : 'Http';
  }
  return scheme === 'Wss' ? 'Https' : 'Http';
}

/** The default scheme pair for a transport on a given host. */
function defaultSchemesForTransport(
  transport: Transport,
  host: string,
): { scheme: Scheme; restScheme: RestScheme } {
  if (transport === 'Quic') {
    return {
      scheme: isLoopbackHost(host) ? 'Quic' : 'QuicTls',
      restScheme: isLoopbackHost(host) ? 'Http' : 'Https',
    };
  }
  const pair = defaultSchemesForHost(host);
  return { scheme: pair.scheme, restScheme: pair.restScheme };
}

/** Builds a {@link ServerEndpoint} from the form's local state. */
export function buildFromForm(state: FormState): ServerEndpoint {
  if (state.host.trim() === '') {
    throw new Error('host is required');
  }
  // Split `host:port` shorthand: `migo.example.com:8443` once. The form takes the host and port
  // as separate fields, but the user can still paste the shorthand into the host field.
  const { host, port: inlinePort } = parseHost(state.host, 18080);
  const port =
    inlinePort !== 18080 && state.port.trim() === ''
      ? inlinePort
      : parsePortNumber(state.port, 'port');
  const gatewayPort = parsePortNumber(state.gatewayPort, 'gateway port');
  validatePorts(port, gatewayPort);
  if (state.transport === 'WebSocket') {
    if (state.scheme !== 'Ws' && state.scheme !== 'Wss') {
      throw new Error('WebSocket transport requires WS or WSS scheme');
    }
    if (state.restScheme !== 'Http' && state.restScheme !== 'Https') {
      throw new Error('REST scheme must be HTTP or HTTPS');
    }
  } else if (state.transport === 'Quic') {
    if (state.scheme !== 'Quic' && state.scheme !== 'QuicTls') {
      throw new Error('QUIC transport requires QUIC or QUIC-TLS scheme');
    }
  }
  return {
    host,
    port,
    gatewayPort,
    transport: state.transport,
    scheme: state.scheme,
    restScheme: state.restScheme,
  };
}

function parsePortNumber(raw: string, label: string): number {
  const trimmed = raw.trim();
  if (trimmed === '') {
    throw new Error(`${label} is required`);
  }
  const value = Number.parseInt(trimmed, 10);
  if (!Number.isInteger(value) || value < 1 || value > 65535) {
    throw new Error(`${label} is out of range (1..65535): ${raw}`);
  }
  if (String(value) !== trimmed) {
    throw new Error(`${label} is not a whole number: ${raw}`);
  }
  return value;
}

// Re-export so the login/register pages do not have to import the type from the SDK.
export type { WsScheme };
