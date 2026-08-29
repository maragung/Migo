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
import { restBaseUrl } from './server-endpoint.js';
import type { ServerEndpoint } from './server-endpoint.js';

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

/**
 * Which rendering a captcha challenge asks the server for.
 *
 * `image` is the ordinary distorted-text challenge; `image_alt` is the accessible
 * alternative — a freshly-issued challenge carrying a *different* random code, rendered
 * with larger glyphs and less noise for the users who cannot read the ordinary one. It
 * is still an image the user has to solve, just a gentler one; the two are answered and
 * verified the same way, so a caller can offer the alt mode without any other change.
 */
export type CaptchaMode = 'image' | 'image_alt';

/**
 * An image captcha challenge the server hands out for the public bootstrap surface.
 *
 * `image_png_base64` is a standard-base64 PNG (padding included) that the caller renders
 * for the user; the answer is whatever the user reads off that rendered image. Nothing
 * about the answer is in this response — the challenge is the picture, and the proof is
 * the user's typing bound to `challenge_id`. `mode` echoes which rendering the server
 * actually issued, so a caller refreshing the challenge can ask for the same one again.
 */
export interface CaptchaChallenge {
  challenge_id: Id;
  image_png_base64: string;
  mode: CaptchaMode;
  ttl_seconds: number;
}

/** A user-supplied captcha answer, bound to the challenge it answers. */
export interface CaptchaProof {
  challenge_id: Id;
  answer: string;
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
  /**
   * When the server returns `CAPTCHA_REQUIRED` for a state it gates, the client supplies the
   * proof on a retry. Omitted on the first attempt.
   */
  captcha?: CaptchaProof;
}

/** A sign-in request. One identifier field because a user does not separate username from email. */
export interface LoginParams {
  identifier: string;
  password: string;
  device: DeviceDescriptor;
  /**
   * When the server returns `CAPTCHA_REQUIRED` for a state it gates, the client supplies the
   * proof on a retry. Omitted on the first attempt.
   */
  captcha?: CaptchaProof;
}

/** A refresh-token exchange, checked against the device the token was minted for. */
export interface RefreshParams {
  refreshToken: string;
  deviceId: Id;
}

/**
 * One row of the sessions list.
 *
 * `id` is the session id (so the caller can revoke it), `device` is the human-readable device
 * name the session was opened on, `created_at` and `last_seen_at` are Unix milliseconds, and
 * `ip_class` is the server's coarse classification of the source address. Named differently from
 * the transport's `SessionInfo` (which describes the gateway session) to keep the two ideas
 * unambiguous: this is the account-side sessions list used by the security page, not the
 * transport-state struct.
 */
export interface AccountSession {
  id: Id;
  device: string;
  created_at: number;
  last_seen_at: number;
  ip_class: number;
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

/** Maps a {@link CaptchaProof} to the snake_case body the server's captcha deserialiser reads. */
function captchaBody(proof: CaptchaProof): Record<string, unknown> {
  return { challenge_id: proof.challenge_id, answer: proof.answer };
}

/**
 * Coerces the wire `mode` field into a {@link CaptchaMode}.
 *
 * The server answers `image` or `image_alt` and nothing else, but this field crosses a
 * network: one that is missing (an older server) or unknown (a newer one) collapses to
 * the default rendering rather than leaking a string the caller's types do not allow.
 */
function parseCaptchaMode(value: unknown): CaptchaMode {
  return value === 'image_alt' ? 'image_alt' : 'image';
}

/** Maps a `AccountSession`-shaped JSON row from the server into the SDK's typed view. */
function parseSessionInfo(row: Record<string, unknown>): AccountSession {
  const deviceField = (row['device'] ?? row['display_name'] ?? '') as string;
  return {
    id: parseId(String(row['id'] ?? row['session_id'])),
    device: deviceField,
    created_at: Number(row['created_at']),
    last_seen_at: Number(row['last_seen_at']),
    ip_class: Number(row['ip_class']),
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
 * Construct it with the user's {@link ServerEndpoint} (host, port, scheme); the REST origin
 * `http(s)://host:port` is derived from it and each method appends its own `/v1/...` path. Every
 * call that fails with a non-2xx status throws a {@link RemoteError} built from the server's error
 * envelope.
 */
export class BootstrapClient {
  readonly #baseUrl: string;
  readonly #fetch: FetchLike;

  constructor(server: ServerEndpoint, options: BootstrapOptions = {}) {
    // Store the origin without a trailing slash so path joins are unambiguous.
    this.#baseUrl = restBaseUrl(server).replace(/\/+$/, '');
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
      ...(params.captcha !== undefined ? { captcha: captchaBody(params.captcha) } : {}),
    };
    return parseGrant(await this.#post('/v1/auth/register', body));
  }

  /** `POST /v1/auth/login` — open a session for an existing account. */
  async login(params: LoginParams): Promise<Grant> {
    const body = {
      identifier: params.identifier,
      password: params.password,
      device: deviceBody(params.device),
      ...(params.captcha !== undefined ? { captcha: captchaBody(params.captcha) } : {}),
    };
    return parseGrant(await this.#post('/v1/auth/login', body));
  }

  /**
   * `POST /v1/auth/captcha` — request an image captcha challenge for the public
   * bootstrap surface.
   *
   * The server mints a short-lived challenge as a PNG the caller renders; the user reads
   * the answer off the image, and that typing becomes the {@link CaptchaProof} on the
   * next register/login attempt. Nothing about the answer crosses the wire here. `mode`
   * selects the rendering: omitted (or `'image'`) asks for the ordinary distorted text,
   * and `'image_alt'` asks for the gentler accessible alternative — a fresh challenge
   * either way, solved identically. Errors are surfaced as {@link RemoteError} like any
   * other bootstrap call; the caller handles `CAPTCHA_REQUIRED` itself, not this method.
   */
  async requestCaptcha(mode?: CaptchaMode): Promise<CaptchaChallenge> {
    const body = (await this.#post(
      '/v1/auth/captcha',
      // An omitted mode is the ordinary challenge; `{ mode }` is how a caller asks for
      // the alt rendering. The server accepts either shape for the default.
      mode === undefined ? {} : { mode },
    )) as Record<string, unknown>;
    return {
      challenge_id: parseId(String(body['challenge_id'])),
      image_png_base64: String(body['image_png_base64']),
      mode: parseCaptchaMode(body['mode']),
      ttl_seconds: Number(body['ttl_seconds']),
    };
  }

  /**
   * `POST /v1/auth/recovery/request` — start a password-recovery flow.
   *
   * The server is deliberately enumeration-safe: a real account and a not-found one both answer
   * `{ ok: true }`. The captcha gate is the rate-limiter's defence; the page is generic.
   */
  async recoverAccount(params: {
    identifier: string;
    captcha: CaptchaProof;
  }): Promise<{ ok: true }> {
    const body = {
      identifier: params.identifier,
      captcha: captchaBody(params.captcha),
    };
    await this.#post('/v1/auth/recovery/request', body);
    return { ok: true };
  }

  /**
   * `POST /v1/auth/recovery/confirm` — apply a recovery token to set a new password.
   *
   * `token_id` is the public id of the recovery grant; `token` is the proof (its hash), supplied
   * out-of-band. On success the new password is set and the caller signs the user in normally.
   */
  async confirmRecovery(params: {
    token_id: Id;
    token: string;
    new_password: string;
  }): Promise<{ ok: true }> {
    const body = {
      token_id: params.token_id,
      token: params.token,
      new_password: params.new_password,
    };
    await this.#post('/v1/auth/recovery/confirm', body);
    return { ok: true };
  }

  /**
   * `GET /v1/auth/sessions` — list every session for the authenticated account.
   *
   * Requires the caller's access token; it is sent as a Bearer credential.
   */
  async listSessions(accessToken: string): Promise<AccountSession[]> {
    const body = (await this.#get('/v1/auth/sessions', accessToken)) as {
      sessions: Record<string, unknown>[];
    };
    return (body.sessions ?? []).map(parseSessionInfo);
  }

  /**
   * `POST /v1/auth/sessions/{id}/revoke` — revoke a single session by id.
   *
   * Requires the caller's access token. Answers `{ ok: true }` on success.
   */
  async revokeSession(accessToken: string, sessionId: Id): Promise<{ ok: true }> {
    await this.#post(`/v1/auth/sessions/${sessionId}/revoke`, {}, accessToken);
    return { ok: true };
  }

  /**
   * `POST /v1/auth/sessions/revoke-others` — revoke every session except the caller's.
   *
   * Returns the number of sessions that were revoked. The caller's own session stays alive.
   */
  async signOutOthers(accessToken: string): Promise<{ revoked: number }> {
    const body = (await this.#post('/v1/auth/sessions/revoke-others', {}, accessToken)) as Record<
      string,
      unknown
    >;
    return { revoked: Number(body['revoked']) };
  }

  /**
   * `POST /v1/auth/password` — change the authenticated account's password.
   *
   * On success the server returns a fresh grant; the caller's existing session and refresh token
   * are replaced with new ones.
   */
  async changePassword(
    accessToken: string,
    params: { current_password: string; new_password: string },
  ): Promise<Grant> {
    return parseGrant(
      await this.#post(
        '/v1/auth/password',
        {
          current_password: params.current_password,
          new_password: params.new_password,
        },
        accessToken,
      ),
    );
  }

  /**
   * `PUT /v1/auth/contact` — record an email or phone so the account can be recovered.
   *
   * The wire layer accepts exactly one of the two; the client-side check in {@link
   * MigoClient.updateContact} keeps callers from sending a request the server will reject.
   */
  async updateContact(
    accessToken: string,
    params: { email_or_phone: string },
  ): Promise<{ ok: true }> {
    await this.#put('/v1/auth/contact', params, accessToken);
    return { ok: true };
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

  /** Issues a JSON PUT, throwing a {@link RemoteError} on any non-2xx answer. */
  async #put(path: string, body: unknown, bearer?: string): Promise<unknown> {
    const headers: Record<string, string> = { 'content-type': 'application/json' };
    if (bearer !== undefined) headers['authorization'] = `Bearer ${bearer}`;
    const res = await this.#fetch(`${this.#baseUrl}${path}`, {
      method: 'PUT',
      headers,
      body: JSON.stringify(body),
    });
    return this.#unwrap(res);
  }

  /** Issues a GET, throwing a {@link RemoteError} on any non-2xx answer. */
  async #get(path: string, bearer?: string): Promise<unknown> {
    const headers: Record<string, string> = {};
    if (bearer !== undefined) headers['authorization'] = `Bearer ${bearer}`;
    const res = await this.#fetch(`${this.#baseUrl}${path}`, { method: 'GET', headers });
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
