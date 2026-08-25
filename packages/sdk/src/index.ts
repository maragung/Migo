/**
 * `@migo/sdk` — the client SDK for the Migo protocol.
 *
 * The one entry point an application imports. {@link MigoClient} is the composition root that brings the
 * whole stack online with a few calls; the individual domains, crypto layers, and wire primitives are
 * exported too, for callers that want to assemble the pieces themselves or reach below the client.
 *
 * A typical application needs only {@link MigoClient}, the {@link ContentType} helpers to build message
 * content, and a handful of protocol enums (re-exported here and, in full, under the {@link protocol}
 * namespace). Everything else is here for advanced use: driving a domain directly, supplying custom key
 * storage, or working with raw frames and envelopes.
 */

// --- the client ---
export { MigoClient, DEFAULT_REPLENISH_POLICY } from './client.js';
export type { MigoClientOptions, ClientHello, PrekeyReplenishPolicy } from './client.js';

// --- bootstrap over REST ---
export { BootstrapClient } from './rest.js';
export type {
  BootstrapOptions,
  ConfigLimits,
  DeviceDescriptor,
  FetchLike,
  Grant,
  LoginParams,
  NodeConfig,
  RefreshParams,
  RegisterParams,
  ServerConfig,
} from './rest.js';

// --- the resumable gateway transport ---
export { GatewayTransport, DEFAULT_CLIENT_FEATURES } from './transport.js';
export type {
  ConnectionState,
  HelloParams,
  SessionInfo,
  TransportOptions,
  WebSocketFactory,
} from './transport.js';

// --- the request/event bridge and per-slice domains ---
export { Rpc } from './domains/rpc.js';
export type { EventErrorHandler } from './domains/rpc.js';
export { ListenerSet } from './domains/listeners.js';
export type { Listener } from './domains/listeners.js';

export { KeyStore, KeysDomain } from './domains/keys.js';
export type { KeyStoreSnapshot, DeviceBundle } from './domains/keys.js';

export { MessagingDomain } from './domains/messaging.js';
export type {
  DeviceAddress,
  DeviceDirectory,
  IncomingMessage,
  MessageDeletion,
  SendOptions,
} from './domains/messaging.js';

export { ConversationsDomain } from './domains/conversations.js';
export type { CreateConversationOptions } from './domains/conversations.js';

export { SyncDomain } from './domains/sync.js';
export type { SyncOptions } from './domains/sync.js';

export { TypingDomain } from './domains/typing.js';
export { PresenceDomain } from './domains/presence.js';
export type { PresenceOptions } from './domains/presence.js';
export { RoomsDomain } from './domains/rooms.js';
export type { RoomListFilter } from './domains/rooms.js';
export { ProfileDomain } from './domains/profile.js';
export { NotificationsDomain } from './domains/notifications.js';
export { GamesDomain } from './domains/games.js';
export type { SubmitOptions } from './domains/games.js';

// --- the two end-to-end crypto policy layers ---
export {
  SessionCrypto,
  ENVELOPE_VERSION,
  SCHEME_DOUBLE_RATCHET,
  SCHEME_DOUBLE_RATCHET_PREKEY,
  SCHEME_SENDER_KEY,
} from './session-crypto.js';
export type { LocalKeyStore, PeerBundleSource, SealedEnvelope } from './session-crypto.js';
export { GroupCrypto } from './group-crypto.js';
export type { IdentityProvider } from './group-crypto.js';

// --- message content (the sealed inner plaintext) ---
export { ContentType, encodeContent, decodeContent, conversationContext } from './content.js';
export type {
  ContentEncodeOptions,
  ControlEventContent,
  MediaRefContent,
  MessageContent,
  ReactionContent,
  TextContent,
  VoiceNoteRefContent,
} from './content.js';

// --- errors, ids, and low-level frame/envelope primitives ---
export { SdkError, RemoteError, TransportError, TimeoutError } from './errors.js';
export { newId } from './ids.js';
export {
  encodeBody,
  decodeBody,
  opcodeMeta,
  opcodeLabel,
  hasErrorFlag,
  requiresAck,
  frameDeliveryClass,
  isSequenced,
} from './codec.js';
export type { BodyEncoder, BodyDecoder } from './codec.js';
export { EnvelopeWriter, EnvelopeReader } from './envelope-buffer.js';

// --- protocol types, re-exported for convenience ---
// The enums an application compares against, and the events and responses the domains return, are surfaced
// at the top level; the entire generated surface (every opcode, struct, and codec) is available under the
// `protocol` namespace for anything not re-exported here.
export {
  BandwidthMode,
  ConversationKind,
  EncryptionMode,
  MessageKind,
  NotificationKind,
  Platform,
  PresenceState,
  ReceiptKind,
  RoomKind,
  RoomRole,
  SyncStatus,
  TopicKind,
  TypingState,
} from '@migo/protocol';
export type {
  Acknowledged,
  ConversationListResponse,
  ConversationSummary,
  GameEvent,
  MessageAccepted,
  MessageEvent,
  MessageReceipt,
  NotificationEvent,
  PresenceEvent,
  RoomJoinResponse,
  RoomListResponse,
  RoomMemberEvent,
  RoomStateEvent,
  RoomSummary,
  SyncResponse,
  Topic,
  TypingEvent,
  UserProfile,
} from '@migo/protocol';
export * as protocol from '@migo/protocol';

// --- wire primitives ---
// `Id` names an account, device, conversation, room, or message everywhere in this API; the rest of the
// wire layer (readers, writers, framing) is available under the `wire` namespace.
export type { Id } from '@migo/wire';
export * as wire from '@migo/wire';
