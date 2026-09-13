'use client';

/**
 * The server picker's body, mounted inside a sheet from the sign-in and registration cards.
 *
 * The card itself stays sparse — identifier and passphrase, nothing else — and the server choice
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
 * save a QUIC-capable server and the rest of the surface (REST, the identity ceremonies) proceeds.
 *
 * # How the server itself is picked
 *
 * When the build names more than one server (`NEXT_PUBLIC_MIGO_SERVERS`), a mode control sits
 * above everything else: "Otomatis" (the default — every server is probed and the fastest
 * responder is used), one explicit pick per known server, and "Manual…" for the typed form. The
 * mode is a draft like the host and port; it commits with the same button. In auto mode the
 * resolution is shown in the sheet — the user sees *which* node the probe chose — and a probe
 * that nothing answered keeps auto mode and says so, because pinning a dead server silently is
 * the one outcome the picker must never produce. A build with a single server shows no mode
 * control at all: there is nothing to choose between, and the form is the manual form it always
 * was.
 */

import { useEffect, useId, useRef, useState } from 'react';
import type { ReactNode } from 'react';

import { defaultSchemesForHost, parseHost, validatePorts } from '@migo/sdk';
import type { RestScheme, Scheme, ServerEndpoint, Transport, WsScheme } from '@migo/sdk';

import { pickFastestServer, withTransport } from '@/lib/auto-server.js';
import type { ServerChoiceMode } from '@/lib/storage/server-endpoint-store.js';

export interface ServerFormProps {
  /** The initial value, e.g. the persisted endpoint or the env default. */
  value: ServerEndpoint;
  /** The committed mode the persisted record carries (auto / server / manual). */
  mode: ServerChoiceMode;
  /**
   * The servers this build names (`knownServers()`); with fewer than two entries the mode
   * control does not render and the form is the plain manual form.
   */
  servers: ServerEndpoint[];
  /**
   * Called when the user has picked an endpoint and confirmed it ("Use this server"). The form
   * does not call this on every keystroke — the host and port fields are local until the button
   * is clicked, so the parent form is not revalidated while the user is still typing. Inside a
   * sheet this is the commit that also closes it.
   */
  onCommit: (next: ServerEndpoint, mode: ServerChoiceMode) => void;
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

/** Where the auto probe stands: not yet run, in flight, answered, or answered by nobody. */
type AutoProbe =
  | { phase: 'idle' }
  | { phase: 'probing' }
  | { phase: 'resolved'; endpoint: ServerEndpoint }
  | { phase: 'unreachable' };

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

/** Whether a known server is the one an endpoint names, by the doors that matter: host and port. */
function sameServer(server: ServerEndpoint, endpoint: ServerEndpoint): boolean {
  return server.host === endpoint.host && server.port === endpoint.port;
}

/**
 * The mode the form opens in. A persisted mode stands; a "server" pick whose node left the
 * build's list opens as manual (the address is still there, just no longer a named radio), and
 * a single-server build is always manual because there is nothing to choose between.
 */
function initialDraftMode(
  mode: ServerChoiceMode,
  servers: ServerEndpoint[],
  value: ServerEndpoint,
): ServerChoiceMode {
  if (servers.length < 2) {
    return 'manual';
  }
  if (mode === 'auto') {
    return 'auto';
  }
  if (mode === 'server' && servers.some((server) => sameServer(server, value))) {
    return 'server';
  }
  return 'manual';
}

/**
 * Renders the picker's body. The draft is local until "Use this server"; the parent's `value` is
 * the only thing the parent cares about, and that only updates on a commit.
 */
export function ServerForm({
  value,
  mode,
  servers,
  onCommit,
  onTransportPick,
}: ServerFormProps): ReactNode {
  const [draft, setDraft] = useState<FormState>(toForm(value));
  const [draftMode, setDraftMode] = useState<ServerChoiceMode>(() =>
    initialDraftMode(mode, servers, value),
  );
  const [draftServer, setDraftServer] = useState<number>(() => {
    const index = servers.findIndex((server) => sameServer(server, value));
    return index >= 0 ? index : 0;
  });
  const [probe, setProbe] = useState<AutoProbe>({ phase: 'idle' });
  const [error, setError] = useState<string | null>(null);
  const hostId = useId();
  const portId = useId();
  const schemeId = useId();
  // The run counter cancels a probe whose mode was switched away before it answered: only the
  // newest run may set state, so a slow answer from an abandoned "Otomatis" pick lands nowhere.
  const probeRun = useRef(0);

  // Auto mode probes on entry (and re-probes when the user returns to it): the resolution the
  // sheet shows must be this visit's, not a cache of the last one.
  useEffect(() => {
    if (draftMode !== 'auto' || servers.length < 2) {
      return;
    }
    const run = ++probeRun.current;
    setProbe({ phase: 'probing' });
    let live = true;
    void pickFastestServer(servers).then(
      (endpoint) => {
        if (live && probeRun.current === run) {
          setProbe(endpoint === null ? { phase: 'unreachable' } : { phase: 'resolved', endpoint });
        }
      },
      () => {
        if (live && probeRun.current === run) {
          setProbe({ phase: 'unreachable' });
        }
      },
    );
    return () => {
      live = false;
    };
  }, [draftMode, servers]);

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
   * rides along, preserving the endpoint's own TLS posture (see {@link withTransport}) so the
   * committed record stays a valid pair — a plain `http://` deployment keeps its plain pair instead
   * of being restamped to a certificate it does not have. The sheet stays open: the pick is
   * reported through `onTransportPick` (or `onCommit` when no separate handler is given) because
   * a transport swap is not the end of the picking session.
   */
  const pickTransport = (transport: Transport): void => {
    if (transport === value.transport) {
      return;
    }
    const next = withTransport(value, transport);
    if (onTransportPick !== undefined) {
      onTransportPick(next);
    } else {
      onCommit(next, mode);
    }
    setDraft(toForm(next));
  };

  /**
   * The commit, per mode. Manual runs the field validation it always did; a server pick commits
   * the named node under the transport already chosen; auto commits the probe's resolution, or —
   * when nothing answered — the endpoint the parent already holds, so the mode stays auto and
   * the sign-in attempt reports the connection failure through the form's normal error path.
   */
  const commit = (): void => {
    if (draftMode === 'manual') {
      try {
        const next = buildFromForm(draft);
        onCommit(next, 'manual');
        setError(null);
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : 'invalid server');
      }
      return;
    }
    if (draftMode === 'server') {
      const picked = servers[draftServer];
      if (picked === undefined) {
        setError('no server selected');
        return;
      }
      onCommit(withTransport(picked, value.transport), 'server');
      setError(null);
      return;
    }
    const resolved = probe.phase === 'resolved' ? probe.endpoint : null;
    // A transport the user already chose rides along onto the freshly probed node, the same way
    // it rides along on every other commit.
    const next = resolved === null ? value : withTransport(resolved, value.transport);
    onCommit(next, 'auto');
    setError(null);
  };

  const pickMode = (next: ServerChoiceMode): void => {
    setDraftMode(next);
    setError(null);
  };

  /** The one-line status the auto radio and its hint share. */
  const autoNote = (): { short: string; long: string } => {
    if (probe.phase === 'probing') {
      return { short: 'checking…', long: `Probing ${servers.length} servers for the fastest…` };
    }
    if (probe.phase === 'resolved') {
      return {
        short: `${probe.endpoint.host}:${probe.endpoint.port}`,
        long: `Using ${probe.endpoint.host}:${probe.endpoint.port} — the fastest of ${servers.length} servers to answer.`,
      };
    }
    if (probe.phase === 'unreachable') {
      return {
        short: 'no server answered',
        long: 'No server answered the probe. Auto stays on; sign-in will be attempted and will report the connection error.',
      };
    }
    return {
      short: 'probes every server',
      long: 'Probes every server and uses the fastest to answer.',
    };
  };

  const showModeControl = servers.length > 1;

  return (
    <div className="server-form">
      {showModeControl ? (
        <div className="server-form-modes" role="radiogroup" aria-label="Server choice">
          <button
            type="button"
            role="radio"
            aria-checked={draftMode === 'auto'}
            className={`server-form-mode${draftMode === 'auto' ? ' active' : ''}`}
            onClick={() => pickMode('auto')}
          >
            <span className="server-form-mode-name">Otomatis</span>
            <span className="server-form-mode-note">{autoNote().short}</span>
          </button>
          {servers.map((server, index) => (
            <button
              type="button"
              role="radio"
              key={`${server.restScheme}://${server.host}:${server.port}`}
              aria-checked={draftMode === 'server' && draftServer === index}
              className={`server-form-mode${draftMode === 'server' && draftServer === index ? ' active' : ''}`}
              onClick={() => {
                setDraftServer(index);
                pickMode('server');
              }}
            >
              <span className="server-form-mode-name">
                {server.host}:{server.port}
              </span>
              <span className="server-form-mode-note">known server</span>
            </button>
          ))}
          <button
            type="button"
            role="radio"
            aria-checked={draftMode === 'manual'}
            className={`server-form-mode${draftMode === 'manual' ? ' active' : ''}`}
            onClick={() => pickMode('manual')}
          >
            <span className="server-form-mode-name">Manual…</span>
            <span className="server-form-mode-note">type the address</span>
          </button>
        </div>
      ) : null}
      {showModeControl && draftMode === 'auto' ? (
        <p className="hint" role="status">
          {autoNote().long}
        </p>
      ) : null}
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
      {draftMode === 'manual' ? (
        <>
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
                onChange={(event) =>
                  setDraft((current) => ({ ...current, port: event.target.value }))
                }
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
        </>
      ) : null}
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
