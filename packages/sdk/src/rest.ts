/**
 * The REST bootstrap: the four auth calls and the one config read a client makes before it can
 * open a socket.
 *
 * A client cannot open the realtime transport without an access token, and it cannot get an access
 * token over a transport it has not opened yet. Section 118 permits exactly this bootstrap over
 * REST and nothing more — once a session is minted, everything else happens on the socket. So this
 * client is deliberately small: {@link BootstrapClient.register}, {@link BootstrapClient.login},
 * {@link BootstrapClient.refresh}, {@link BootstrapClient.logout}, and {@link BootstrapClient.config}.
 *
 * The wire format here is REST-native JSON in snake_case, which is not the camelCase the protocol
 * structs use on the socket — so this module maps between the two by hand rather than sharing the
 * MSE codec. Ids cross as 26-char Crockford base32 text and are parsed back with {@link parseId}.
 * The token fields in a {@link Grant} are the caller's own credentials, returned to the caller that
 * just proved its identity; a caller may hold them but must never write them to a log (section 145).
 */

import { parseId } from '@migo/wire';
import type { Id } from '@migo/wire';
import { Platform } from '@migo/protocol';

import { RemoteError } from './errors.js';

/** The platform names the server's `parse_platform` recognises; anything else maps to Unknown. */
const PLATFORM_NAME: Partial<Record<Platform, string>> = {
  [Platform.Web]: 'web',
  [Platform.Android]: 'android',
  [Platform.Ios]: 'ios',
  [Platform.Desktop]: 'desktop',
  [Platform.Bot]: 'bot',
};

/**
 * What a client claims about the device a session runs on.
 *
 * Every field is a claim and none of it grants anything — the server binds a session to a device
 * by its own rules. `deviceId` is omitted on a first sign-in and supplied on later ones to reclaim
 * the same device identity.
 */
export interface DeviceDescriptor {
  platform: Platform;
  displayName: string;
  deviceId?: Id;
  appVersion?: string;
  osVersion?: string;
  deviceModel?: string;
}

/** A new-account request. `locale` defaults to the server's own default when omitted. */
export interface RegisterParams {
  username: string;
  password: string;
  email?: string;
  phone?: string;
  locale?: string;
  country?: string;
  device: DeviceDescriptor;
}

/** A sign-in request. One identifier field because a user does not separate username from email. */
export interface LoginParams {
  identifier: string;
  password: string;
  device: DeviceDescriptor;
}

/** A refresh-token exchange, checked against the device the token was minted for. */
export interface RefreshParams {
  refreshToken: string;
  deviceId: Id;
}

/**
 * The session a successful bootstrap yields.
 *
 * `accessToken` opens the socket (it rides in HELLO or AUTHENTICATE); `refreshToken` buys a fresh
 * session when the access token expires. `capabilities` is the same 64-bit grant the socket's
 * `Authenticated` frame carries, so it is a `bigint`.
 */
export interface Grant {
  accountId: Id;
  deviceId: Id;
  sessionId: Id;
  accessToken: string;
  refreshToken: string;
  accessExpiresAtMs: number;
  refreshExpiresAtMs: number;
  capabilities: bigint;
  isNewAccount: boolean;
}

/** The node's identity, as a client needs to display and route to it. */
export interface NodeConfig {
  id: string;
  region: string;
  country: string;
  publicUrl: string;
}

/** The policy limits a client validates its own forms against before sending a doomed request. */
export interface ConfigLimits {
  allowRegistration: boolean;
  passwordMinLength: number;
  maxDevicesPerUser: number;
  maxBodyBytes: number;
  maxPageSize: number;
}

/** The runtime configuration document read once at startup. */
export interface ServerConfig {
  node: NodeConfig;
  features: bigint;
  limits: ConfigLimits;
}

/** The `fetch` signature this client needs, so a caller can inject one in an environment without a global. */
export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

/** Options for constructing a {@link BootstrapClient}. */
export interface BootstrapOptions {
  /** The `fetch` to use; defaults to the global one. */
  fetch?: FetchLike;
}

/**
 * Coerces a JSON number, numeric string, or bigint to a bigint.
 *
 * The server serialises a `u64` bitset (`capabilities`, `features`) as a bare JSON number. Every
 * feature and capability bit in use fits well inside `Number.MAX_SAFE_INTEGER`, so the conversion
 * is exact today; accepting a string too means a future server that widens the set by emitting a
 * string needs no client change.
 */
function asBigInt(value: unknown): bigint {
  if (typeof value === 'bigint') return value;
  if (typeof value === 'number' && Number.isInteger(value)) return BigInt(value);
  if (typeof value === 'string' && value !== '') return BigInt(value);
  return 0n;
}

/** Maps a device descriptor to the snake_case JSON body the server's `DeviceRequest` deserialises. */
function deviceBody(device: DeviceDescriptor): Record<string, unknown> {
  const platform = PLATFORM_NAME[device.platform];
  return {
    display_name: device.displayName,
    ...(platform !== undefined ? { platform } : {}),
    ...(device.deviceId !== undefined ? { device_id: device.deviceId } : {}),
    ...(device.appVersion !== undefined ? { app_version: device.appVersion } : {}),
    ...(device.osVersion !== undefined ? { os_version: device.osVersion } : {}),
    ...(device.deviceModel !== undefined ? { device_model: device.deviceModel } : {}),
  };
}

/** Maps a `GrantResponse` JSON body into a {@link Grant}, parsing its Id text fields. */
function parseGrant(body: unknown): Grant {
  const g = body as Record<string, unknown>;
  return {
    accountId: parseId(String(g['account_id'])),
    deviceId: parseId(String(g['device_id'])),
    sessionId: parseId(String(g['session_id'])),
    accessToken: String(g['access_token']),
    refreshToken: String(g['refresh_token']),
    accessExpiresAtMs: Number(g['access_expires_at_ms']),
    refreshExpiresAtMs: Number(g['refresh_expires_at_ms']),
    capabilities: asBigInt(g['capabilities']),
    isNewAccount: Boolean(g['is_new_account']),
  };
}

/**
 * The REST bootstrap client.
 *
 * Construct it with the node's base URL (the origin, e.g. `https://node.example`); each method
 * appends its own `/v1/...` path. Every call that fails with a non-2xx status throws a
 * {@link RemoteError} built from the server's error envelope.
 */
export class BootstrapClient {
  readonly #baseUrl: string;
  readonly #fetch: FetchLike;

  constructor(baseUrl: string, options: BootstrapOptions = {}) {
    // Store the origin without a trailing slash so path joins are unambiguous.
    this.#baseUrl = baseUrl.replace(/\/+$/, '');
    const fetchImpl = options.fetch ?? globalThis.fetch;
    if (fetchImpl === undefined) {
      throw new TypeError(
        'BootstrapClient needs a fetch implementation: none was found on globalThis',
      );
    }
    // Bind so a global `fetch` is not called with the wrong receiver.
    this.#fetch = options.fetch ?? fetchImpl.bind(globalThis);
  }

  /** `POST /v1/auth/register` — create an account and open its first session. */
  async register(params: RegisterParams): Promise<Grant> {
    const body = {
      username: params.username,
      password: params.password,
      device: deviceBody(params.device),
      ...(params.email !== undefined ? { email: params.email } : {}),
      ...(params.phone !== undefined ? { phone: params.phone } : {}),
      ...(params.locale !== undefined ? { locale: params.locale } : {}),
      ...(params.country !== undefined ? { country: params.country } : {}),
    };
    return parseGrant(await this.#post('/v1/auth/register', body));
  }

  /** `POST /v1/auth/login` — open a session for an existing account. */
  async login(params: LoginParams): Promise<Grant> {
    const body = {
      identifier: params.identifier,
      password: params.password,
      device: deviceBody(params.device),
    };
    return parseGrant(await this.#post('/v1/auth/login', body));
  }

  /** `POST /v1/auth/refresh` — exchange a refresh token for a fresh session. */
  async refresh(params: RefreshParams): Promise<Grant> {
    const body = { refresh_token: params.refreshToken, device_id: params.deviceId };
    return parseGrant(await this.#post('/v1/auth/refresh', body));
  }

  /**
   * `POST /v1/auth/logout` — end the named session.
   *
   * Requires the caller's own access token, sent as a Bearer credential. Answers 204 on success,
   * so there is nothing to return.
   */
  async logout(accessToken: string, sessionId: Id): Promise<void> {
    await this.#post('/v1/auth/logout', { session_id: sessionId }, accessToken);
  }

  /** `GET /v1/config` — the node identity, feature bits, and policy limits, unauthenticated. */
  async config(): Promise<ServerConfig> {
    const body = (await this.#get('/v1/config')) as Record<string, unknown>;
    const node = (body['node'] ?? {}) as Record<string, unknown>;
    const limits = (body['limits'] ?? {}) as Record<string, unknown>;
    return {
      node: {
        id: String(node['id']),
        region: String(node['region']),
        country: String(node['country']),
        publicUrl: String(node['public_url']),
      },
      features: asBigInt(body['features']),
      limits: {
        allowRegistration: Boolean(limits['allow_registration']),
        passwordMinLength: Number(limits['password_min_length']),
        maxDevicesPerUser: Number(limits['max_devices_per_user']),
        maxBodyBytes: Number(limits['max_body_bytes']),
        maxPageSize: Number(limits['max_page_size']),
      },
    };
  }

  /** Issues a JSON POST, throwing a {@link RemoteError} on any non-2xx answer. */
  async #post(path: string, body: unknown, bearer?: string): Promise<unknown> {
    const headers: Record<string, string> = { 'content-type': 'application/json' };
    if (bearer !== undefined) headers['authorization'] = `Bearer ${bearer}`;
    const res = await this.#fetch(`${this.#baseUrl}${path}`, {
      method: 'POST',
      headers,
      body: JSON.stringify(body),
    });
    return this.#unwrap(res);
  }

  /** Issues a GET, throwing a {@link RemoteError} on any non-2xx answer. */
  async #get(path: string): Promise<unknown> {
    const res = await this.#fetch(`${this.#baseUrl}${path}`, { method: 'GET' });
    return this.#unwrap(res);
  }

  /**
   * Turns a `Response` into its parsed JSON, or a {@link RemoteError} on failure.
   *
   * A 204 (logout) carries no body and returns `null`. A non-2xx status is read as the error
   * envelope; if the body is not JSON, {@link RemoteError.fromEnvelope} still produces a coherent
   * `HTTP <status>` error rather than throwing a parse fault of its own.
   */
  async #unwrap(res: Response): Promise<unknown> {
    if (res.status === 204) return null;
    let parsed: unknown = null;
    try {
      parsed = await res.json();
    } catch {
      parsed = null;
    }
    if (!res.ok) {
      throw RemoteError.fromEnvelope(res.status, parsed);
    }
    return parsed;
  }
}
