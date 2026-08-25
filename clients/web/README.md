# @migo/web

Migo's web client: a Next.js (App Router) Progressive Web App that speaks to the Migo server through the
`@migo/sdk` package. It is a thin, presentational layer over the SDK — all cryptography, transport, and
protocol logic lives in the SDK and its dependencies, never here.

## What it does

- Register a new account or sign in to an existing one.
- Resume a session automatically on a return visit (see "Persistence" below).
- List conversations, open one, and exchange end-to-end encrypted text messages in real time.
- Show typing indicators and presence for direct conversations.
- Install to the home screen and load its shell offline (PWA).

## Requirements

- Node.js >= 22.11
- pnpm 9 (this app is part of the repository's pnpm workspace)
- A running Migo server reachable over HTTP (REST) and WebSocket (gateway).

## Configuration

All configuration is public and read from `NEXT_PUBLIC_*` environment variables at build time. Copy the
example file and adjust it:

```
cp .env.example .env.local
```

| Variable                       | Default                  | Meaning                                               |
| ------------------------------ | ------------------------ | ----------------------------------------------------- |
| `NEXT_PUBLIC_MIGO_API_URL`     | `http://localhost:8080`  | REST base URL for bootstrap (register/login/refresh). |
| `NEXT_PUBLIC_MIGO_GATEWAY_URL` | `ws://localhost:8080/ws` | WebSocket gateway URL for realtime traffic.           |
| `NEXT_PUBLIC_MIGO_APP_VERSION` | `0.1.0`                  | Reported to the server in the client hello.           |

There are no secret variables. This client holds no server secrets and never should: everything it needs
is either public configuration or the user's own credentials, which stay on the device.

## Develop

From the repository root (so the workspace packages resolve):

```
pnpm install
pnpm --filter @migo/web dev
```

Then open http://localhost:3000. In development the service worker is intentionally not registered, so
code changes are never served stale.

## Build

```
pnpm --filter @migo/web build
pnpm --filter @migo/web start
```

The build uses `output: 'standalone'`, so it can be containerized without the full `node_modules` tree.

Type-check without emitting:

```
pnpm --filter @migo/web typecheck
```

## How it is put together

- `src/lib/migo/provider.tsx` — the single place a `MigoClient` is constructed, brought online, and torn
  down. It owns register/login/logout and the resume-on-mount flow.
- `src/lib/migo/conversations-provider.tsx` — shared conversation-list state for the authenticated shell:
  ordering, unread marks, and live reordering from the message stream.
- `src/lib/migo/use-chat.ts` — per-conversation message state: history catch-up, live delivery, receipts,
  typing, and optimistic echo of the messages this client sends.
- `src/lib/migo/use-profiles.ts`, `use-presence.ts` — public profile and presence resolution with caching.
- `src/components/*` — presentational pieces (sidebar, conversation list, thread window, composer, etc.).
- `src/app/*` — routes: `/login`, `/register`, and the authenticated `/chat` shell with `/chat/[id]`.

### Persistence

The client persists exactly two things, both in **IndexedDB** (never `localStorage`, `sessionStorage`, or
cookies):

- the key-store snapshot — this device's private identity and its message-decryption state, and
- the session grant — the tokens that authorize the session.

The key-store snapshot is re-saved after every operation that can mutate it (establishing a session,
receiving a message, replenishing prekeys, a session reset). On a return visit the grant is refreshed over
REST if its access token has expired, then the socket is opened and history is replayed through the same
decryption path as live delivery.

Private keys are generated on this device and never leave it. The server stores no plaintext and holds no
key that can decrypt messages.

## Known limitation: starting a conversation

The SDK currently exposes no username or directory search RPC, so the "New conversation" dialog accepts
**account IDs** directly (one for a direct conversation, several for a group). Until a directory lookup is
available, obtain a contact's account ID out of band and paste it in. When a peer starts a conversation
with you, it appears in your list automatically — no ID entry needed on your side.
