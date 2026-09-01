'use client';

/**
 * The server picker's body, mounted inside a sheet from the sign-in and registration cards.
 *
 * The card itself stays sparse — identifier and password, nothing else — and the server choice
 * lives behind one small link in the card's bottom corner. The sheet that opens is where the
 * host, port, and scheme are picked; on "Use this server" the caller's endpoint becomes the new
 * form input and the sheet closes.
 *
 * The transport is the one choice that is not a draft: a segmented control shows WebSocket (the
 * default) and QUIC at all times, and one tap commits the swap immediately — a transport change
 * never needs the host and port re-confirmed, so it never lives in the draft. QUIC is a real
 * second option, not a placeholder: the choice persists and is validated the same as WebSocket.
 * Connecting over it requires a server with the QUIC listener enabled, and this client's wire
 * path is still WebSocket. The form accepts the choice and never blocks submit, so the user can
 * save a QUIC-capable server and the rest of the surface (REST, captcha) proceeds.
 */

import { useId, useState } from 'react';
import type { ReactNode } from 'react';

import { defaultSchemesForHost, isLoopbackHost, parseHost, validatePorts } from '@migo/sdk';
import type { RestScheme, Scheme, ServerEndpoint, Transport, WsScheme } from '@migo/sdk';

export interface ServerFormProps {
  /** The initial value, e.g. the persisted endpoint or the env default. */
  value: ServerEndpoint;
  /**
   * Called when the user has picked an endpoint and confirmed it ("Use this server"). The form
   * does not call this on every keystroke — the host and port fields are local until the button
   * is clicked, so the parent form is not revalidated while the user is still typing. Inside a
   * sheet this is the commit that also closes it.
   */
  onCommit: (next: ServerEndpoint) => void;
  /**
   * A transport tap, which commits immediately without the host and port being re-confirmed.
   * Inside a sheet this is what keeps the sheet open: swapping transport is not a reason to end
   * the picking session. Falls back to `onCommit` when omitted.
   */
  onTransportPick?: (next: ServerEndpoint) => void;
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

/** The transport's display name, shared by the card's summary link and the segmented control. */
export function transportLabel(transport: Transport): string {
  return transport === 'Quic' ? 'QUIC' : 'WebSocket';
}

/**
 * Renders the picker's body. The draft is local until "Use this server"; the parent's `value` is
 * the only thing the parent cares about, and that only updates on a commit.
 */
export function ServerForm({ value, onCommit, onTransportPick }: ServerFormProps): ReactNode {
  const [draft, setDraft] = useState<FormState>(toForm(value));
  const [error, setError] = useState<string | null>(null);
  const hostId = useId();
  const portId = useId();
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

  /**
   * Commits a transport swap on the committed endpoint, immediately. The transport is the one
   * field that never lives in the draft: switching it does not need the host and port re-confirmed,
   * so the user does not have to press "Use this server" to make the choice stick. The scheme pair
   * rides along (WebSocket restores the host's WS/WSS pair, QUIC picks QUIC/QUIC-TLS by the same
   * loopback rule) so the committed record stays a valid pair. The sheet stays open: the pick is
   * reported through `onTransportPick` (or `onCommit` when no separate handler is given) because
   * a transport swap is not the end of the picking session.
   */
  const pickTransport = (transport: Transport): void => {
    if (transport === value.transport) {
      return;
    }
    const pair = defaultSchemesForTransport(transport, value.host);
    const next: ServerEndpoint = {
      ...value,
      transport,
      scheme: pair.scheme,
      restScheme: pair.restScheme,
    };
    (onTransportPick ?? onCommit)(next);
    setDraft(toForm(next));
  };

  const commit = (): void => {
    try {
      const next = buildFromForm(draft);
      onCommit(next);
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : 'invalid server');
    }
  };

  return (
    <div className="server-form">
      <div className="server-form-transport">
        <span className="server-form-label">Transport</span>
        <div className="segmented" role="group" aria-label="Realtime transport">
          <button
            type="button"
            className={value.transport === 'WebSocket' ? 'active' : ''}
            aria-pressed={value.transport === 'WebSocket'}
            onClick={() => pickTransport('WebSocket')}
          >
            WebSocket
          </button>
          <button
            type="button"
            className={value.transport === 'Quic' ? 'active' : ''}
            aria-pressed={value.transport === 'Quic'}
            onClick={() => pickTransport('Quic')}
          >
            QUIC
          </button>
        </div>
      </div>
      {value.transport === 'Quic' ? (
        <p className="hint" role="status">
          QUIC is a second option; it needs a server with the QUIC listener enabled. This client
          still connects over WebSocket.
        </p>
      ) : null}
      <div className="server-form-row">
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
            onChange={(event) => setDraft((current) => ({ ...current, port: event.target.value }))}
          />
        </label>
      </div>
      <div className="server-form-row">
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
      {error ? <p className="form-error">{error}</p> : null}
      <div className="form-actions">
        <button type="button" className="btn btn-primary" onClick={commit}>
          Use this server
        </button>
      </div>
    </div>
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
