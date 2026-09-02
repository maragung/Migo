/**
 * The REST bootstrap: the four auth calls and the one config read a client makes before it can
 * open a socket.
 *
 * A client cannot open the realtime transport without an access token, and it cannot get an access
 * token over a transport it has not opened yet. Section 118 permits exactly this bootstrap over
 * REST and nothing more — once a session is minted, everything else happens on the socket. Beyond
 * the password bootstrap ({@link BootstrapClient.register}, {@link BootstrapClient.login},
 * {@link BootstrapClient.refresh}, {@link BootstrapClient.logout}, {@link BootstrapClient.config})
 * it also carries the account-management calls that sit beside a session — sessions, password,
 * recovery, contact — and the ML-DSA identity ceremonies with the device and wallet registry they
 * unlock, which a client holding a `.migo` account root uses instead of a password (§182).
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
   * The account identity's ML-DSA-65 public key, when the registering device already holds the
   * account root it is about to found. Sending it is what makes registration idempotent
   * (brief §12): a retry that carries the same key reconciles into the account the first attempt
   * already made, instead of being refused as a taken name.
   */
  identityPublicKey?: Uint8Array;
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

/**
 * An issued ML-DSA identity challenge: the canonical bytes to sign, and the device the answer is
 * bound to.
 *
 * `payload` is the server's canonical challenge, already base64-decoded to the exact bytes the
 * caller signs — a client signs it as given and never re-encodes it, which is what keeps the ports
 * from disagreeing about what was signed. (Android's mirror keeps the base64 text and decodes at
 * the call site; the SDK decodes here, the same way {@link parseId} turns id text into an {@link Id},
 * so a caller hands the bytes straight to `account.IdentityKey.signLogin`.) `expiresAtMs` is
 * display material for a "challenge expired" message, nothing more.
 */
export interface IdentityChallenge {
  payload: Uint8Array;
  challengeId: Id;
  deviceId: Id;
  expiresAtMs: number;
}

/**
 * One device of the caller's account, for their own security screen.
 *
 * Metadata only — `hasCredential` is whether the device can take part in the ML-DSA login ceremony,
 * never the credential itself. Timestamps are Unix milliseconds.
 */
export interface DeviceSummary {
  deviceId: Id;
  displayName: string;
  platform: string;
  status: string;
  createdAtMs: number;
  lastSeenAtMs: number;
  hasCredential: boolean;
  isCurrent: boolean;
}

/**
 * One registered wallet, for the caller's own wallet list.
 *
 * Address and metadata only; the private key behind it never leaves the device. `address` is the
 * canonical lowercase hex without a `0x` prefix, `label` and `archivedAtMs` are present only when
 * set, and timestamps are Unix milliseconds.
 */
export interface WalletSummary {
  walletId: Id;
  address: string;
  chainType: string;
  label?: string;
  derivationIndex: number;
  status: string;
  createdAtMs: number;
  archivedAtMs?: number;
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

/** The standard base64 alphabet (padded), the encoding the server's `STANDARD` engine reads and writes. */
const BASE64_ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
const BASE64_LOOKUP: ReadonlyMap<string, number> = new Map(
  Array.from(BASE64_ALPHABET, (ch, index) => [ch, index] as const),
);

/**
 * Encodes bytes as standard base64 with padding — the form the identity routes expect for every
 * signature and public key. Written out rather than reaching for `btoa`/`Buffer`, so it behaves the
 * same in a browser, a worker, and Node, and produces exactly what the server's `STANDARD` decoder
 * reads.
 */
function toBase64(bytes: Uint8Array): string {
  let out = '';
  let i = 0;
  for (; i + 2 < bytes.length; i += 3) {
    const n = ((bytes[i] ?? 0) << 16) | ((bytes[i + 1] ?? 0) << 8) | (bytes[i + 2] ?? 0);
    out +=
      BASE64_ALPHABET.charAt((n >> 18) & 63) +
      BASE64_ALPHABET.charAt((n >> 12) & 63) +
      BASE64_ALPHABET.charAt((n >> 6) & 63) +
      BASE64_ALPHABET.charAt(n & 63);
  }
  const remaining = bytes.length - i;
  if (remaining === 1) {
    const n = (bytes[i] ?? 0) << 16;
    out += `${BASE64_ALPHABET.charAt((n >> 18) & 63)}${BASE64_ALPHABET.charAt((n >> 12) & 63)}==`;
  } else if (remaining === 2) {
    const n = ((bytes[i] ?? 0) << 16) | ((bytes[i + 1] ?? 0) << 8);
    out += `${BASE64_ALPHABET.charAt((n >> 18) & 63)}${BASE64_ALPHABET.charAt((n >> 12) & 63)}${BASE64_ALPHABET.charAt((n >> 6) & 63)}=`;
  }
  return out;
}

/**
 * Decodes standard base64 (padding and stray whitespace tolerated) into bytes. Used for the one
 * field that arrives base64 and must be consumed as bytes — a challenge {@link IdentityChallenge.payload}
 * the caller signs.
 */
function fromBase64(text: string): Uint8Array {
  const clean = text.replace(/[^A-Za-z0-9+/]/g, '');
  const out = new Uint8Array((clean.length * 3) >> 2);
  let outIndex = 0;
  let buffer = 0;
  let bits = 0;
  for (const ch of clean) {
    const value = BASE64_LOOKUP.get(ch);
    if (value === undefined) {
      continue;
    }
    buffer = (buffer << 6) | value;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[outIndex] = (buffer >> bits) & 0xff;
      outIndex += 1;
    }
  }
  return out.subarray(0, outIndex);
}

/** Maps a `ChallengeViewBody` JSON body into an {@link IdentityChallenge}, decoding its payload. */
function parseIdentityChallenge(body: unknown): IdentityChallenge {
  const b = body as Record<string, unknown>;
  return {
    payload: fromBase64(String(b['payload'])),
    challengeId: parseId(String(b['challenge_id'])),
    deviceId: parseId(String(b['device_id'])),
    expiresAtMs: Number(b['expires_at_ms']),
  };
}

/** Maps a `DeviceSummary` JSON row into the SDK's typed view. */
function parseDeviceSummary(row: Record<string, unknown>): DeviceSummary {
  return {
    deviceId: parseId(String(row['device_id'])),
    displayName: String(row['display_name']),
    platform: String(row['platform']),
    status: String(row['status']),
    createdAtMs: Number(row['created_at_ms']),
    lastSeenAtMs: Number(row['last_seen_at_ms']),
    hasCredential: Boolean(row['has_credential']),
    isCurrent: Boolean(row['is_current']),
  };
}

/** Maps a `WalletSummary` JSON row into the SDK's typed view, omitting the fields the server omits. */
function parseWalletSummary(row: Record<string, unknown>): WalletSummary {
  const label = row['label'];
  const archivedAt = row['archived_at_ms'];
  return {
    walletId: parseId(String(row['wallet_id'])),
    address: String(row['address']),
    chainType: String(row['chain_type']),
    derivationIndex: Number(row['derivation_index']),
    status: String(row['status']),
    createdAtMs: Number(row['created_at_ms']),
    ...(typeof label === 'string' ? { label } : {}),
    ...(archivedAt !== undefined && archivedAt !== null
      ? { archivedAtMs: Number(archivedAt) }
      : {}),
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
      ...(params.identityPublicKey !== undefined
        ? { identity_public_key: toBase64(params.identityPublicKey) }
        : {}),
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

  // --- the ML-DSA identity ceremonies ----------------------------------------
  //
  // The second front door to a session (§182): a client holding a `.migo` account root asks for a
  // challenge, signs the canonical bytes it is given with both the account identity key and the
  // device credential, and answers with the signatures. Signatures and public keys cross the wire
  // as standard base64; the payload to sign is decoded for the caller (see {@link IdentityChallenge}).

  /**
   * `POST /v1/auth/identity/challenge` (login) — a challenge bound to a registered device.
   *
   * `identifier` names the account (username or email) and `deviceId` the registered device the
   * answer will be signed on; both are required for a login challenge.
   */
  async identityLoginChallenge(params: {
    identifier: string;
    deviceId: Id;
  }): Promise<IdentityChallenge> {
    const body = {
      purpose: 'login',
      identifier: params.identifier,
      device_id: params.deviceId,
    };
    return parseIdentityChallenge(await this.#post('/v1/auth/identity/challenge', body));
  }

  /**
   * `POST /v1/auth/identity/challenge` (add-device) — a challenge for restoring the account onto a
   * new device from a `.migo` container.
   *
   * `accountId` comes from the opened container; `device` describes the new device the restored
   * session will run on.
   */
  async addDeviceChallenge(params: {
    accountId: Id;
    device: DeviceDescriptor;
  }): Promise<IdentityChallenge> {
    const body = {
      purpose: 'add-device',
      account_id: params.accountId,
      device: deviceBody(params.device),
    };
    return parseIdentityChallenge(await this.#post('/v1/auth/identity/challenge', body));
  }

  /**
   * `POST /v1/auth/identity/login` — answer a login challenge with both signatures and receive a
   * session.
   *
   * The login ceremony requires the account identity signature *and* the device credential
   * signature, each over the challenge payload under its own context, so a leaked root alone cannot
   * sign in as a still-registered device.
   */
  async identityLogin(params: {
    challengeId: Id;
    identitySignature: Uint8Array;
    deviceSignature: Uint8Array;
  }): Promise<Grant> {
    const body = {
      challenge_id: params.challengeId,
      identity_signature: toBase64(params.identitySignature),
      device_signature: toBase64(params.deviceSignature),
    };
    return parseGrant(await this.#post('/v1/auth/identity/login', body));
  }

  /**
   * `POST /v1/auth/identity/add-device` — answer an add-device challenge: introduce the new
   * device's credential public key and its signature, and receive the restored session.
   */
  async addDevice(params: {
    challengeId: Id;
    identitySignature: Uint8Array;
    devicePublicKey: Uint8Array;
    deviceSignature: Uint8Array;
  }): Promise<Grant> {
    const body = {
      challenge_id: params.challengeId,
      identity_signature: toBase64(params.identitySignature),
      device_public_key: toBase64(params.devicePublicKey),
      device_signature: toBase64(params.deviceSignature),
    };
    return parseGrant(await this.#post('/v1/auth/identity/add-device', body));
  }

  /**
   * `POST /v1/auth/identity/rotate/challenge` — ask, as the caller's own authenticated device, for
   * a rotation challenge. Requires the caller's access token.
   */
  async rotationChallenge(accessToken: string): Promise<IdentityChallenge> {
    return parseIdentityChallenge(
      await this.#post('/v1/auth/identity/rotate/challenge', {}, accessToken),
    );
  }

  /**
   * `POST /v1/auth/identity/rotate` — answer a rotation challenge with the current key's signature
   * (under the rotate context) and the successor's public key. Answers 204, so there is nothing to
   * return.
   */
  async rotateIdentity(
    accessToken: string,
    params: { challengeId: Id; signature: Uint8Array; newPublicKey: Uint8Array },
  ): Promise<void> {
    const body = {
      challenge_id: params.challengeId,
      signature: toBase64(params.signature),
      new_public_key: toBase64(params.newPublicKey),
    };
    await this.#post('/v1/auth/identity/rotate', body, accessToken);
  }

  /**
   * `POST /v1/auth/identity/key` — publish the caller's identity (and optionally device) public
   * keys on a password-era account: the legacy upgrade door, idempotent by design. Answers 204.
   */
  async publishIdentityKey(
    accessToken: string,
    params: { identityPublicKey: Uint8Array; devicePublicKey?: Uint8Array },
  ): Promise<void> {
    const body = {
      identity_public_key: toBase64(params.identityPublicKey),
      ...(params.devicePublicKey !== undefined
        ? { device_public_key: toBase64(params.devicePublicKey) }
        : {}),
    };
    await this.#post('/v1/auth/identity/key', body, accessToken);
  }

  // --- the device and wallet surfaces ----------------------------------------
  //
  // The authenticated read/write surface of the account's own metadata. Nothing here moves a secret
  // in either direction: the device list carries a public key's presence, and the wallet registry
  // carries an address.

  /** `GET /v1/devices` — the caller's own devices, for their security screen. */
  async devices(accessToken: string): Promise<DeviceSummary[]> {
    const body = (await this.#get('/v1/devices', accessToken)) as {
      devices?: Record<string, unknown>[];
    };
    return (body.devices ?? []).map(parseDeviceSummary);
  }

  /**
   * `POST /v1/devices/{id}/revoke` — remove one of the caller's devices.
   *
   * The device can no longer authenticate, refresh, or open a WebSocket, and every session on
   * it ends. Returns how many sessions died with it, so a security screen can confirm what
   * "this phone is gone" actually removed.
   */
  async revokeDevice(accessToken: string, deviceId: Id): Promise<{ revoked: number }> {
    const body = (await this.#post(`/v1/devices/${deviceId}/revoke`, {}, accessToken)) as Record<
      string,
      unknown
    >;
    return { revoked: Number(body['revoked']) };
  }

  /** `GET /v1/wallets` — the caller's registered wallet addresses. */
  async wallets(accessToken: string): Promise<WalletSummary[]> {
    const body = (await this.#get('/v1/wallets', accessToken)) as {
      wallets?: Record<string, unknown>[];
    };
    return (body.wallets ?? []).map(parseWalletSummary);
  }

  /**
   * `PUT /v1/wallets` — register (or idempotently re-register) a wallet address on the caller's
   * account.
   *
   * `chainType` defaults to `"evm"`, the only chain this release supports; `label` is optional and
   * `derivationIndex` is the `i` in `m/44'/60'/0'/0/i`, so a restore re-registers in order.
   */
  async registerWallet(
    accessToken: string,
    params: { address: string; derivationIndex: number; chainType?: string; label?: string },
  ): Promise<WalletSummary> {
    const body = {
      address: params.address,
      chain_type: params.chainType ?? 'evm',
      derivation_index: params.derivationIndex,
      ...(params.label !== undefined ? { label: params.label } : {}),
    };
    return parseWalletSummary(
      (await this.#put('/v1/wallets', body, accessToken)) as Record<string, unknown>,
    );
  }

  /** `POST /v1/wallets/{wallet_id}` — archive one of the caller's wallets. Answers 204. */
  async archiveWallet(accessToken: string, walletId: Id): Promise<void> {
    await this.#post(`/v1/wallets/${walletId}`, {}, accessToken);
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
