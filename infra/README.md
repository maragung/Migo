# Migo infrastructure

Everything needed to build and run Migo — the `migod` server, the group-call media
plane (`migosfud`), and the web client — as containers, plus a local development
stack that wires them to Postgres and Redis.

```
infra/
  docker/      image definitions (Dockerfile.migod, Dockerfile.sfu, Dockerfile.web)
  compose/     the local development stack (docker-compose.yml)
  kubernetes/  reserved for cluster manifests
  terraform/   reserved for cloud provisioning
```

`migod` and `migosfud` are two images built from one workspace because they are two
deployments with two load profiles (section 92): a session process scales with
connections and a forwarding process scales with bandwidth, and the forwarding one
holds no call key, opens no database, and would have nothing to do with either. See
[the two services](#the-media-plane-is-a-second-process) below.

## Quick start — the full stack

From the repository root:

```sh
docker compose -f infra/compose/docker-compose.yml up --build
```

This builds all three images and starts five services in dependency order:

| Service    | Purpose                                           | Address                |
| ---------- | ------------------------------------------------- | ---------------------- |
| `postgres` | durable store; `migod` migrates it on boot        | internal               |
| `redis`    | cache and rate-limiter backend (no persistence)   | internal               |
| `migod`    | server: REST `/v1`, gateway `/ws`, probes at root | http://localhost:8080  |
| `sfu`      | the group-call media plane: QUIC + `/metrics`     | udp/19443, :9090       |
| `web`      | the static web client (no server-side rendering)  | http://localhost:19992 |

Open http://localhost:19992. Registration is enabled, so you can create an account
and sign in immediately.

Health checks gate the ordering: `migod` starts only once Postgres and Redis report
healthy, and `web` starts only once `migod` reports healthy. The first build compiles
the Rust workspace and can take several minutes; later builds reuse the cached
dependency layer.

This stack is not just documented, it is tested: `make infra-smoke` (run by the
Nightly workflow in CI) builds all three images from the current tree, boots the
stack with the command above, waits for it to be healthy, and asserts on behaviour —
the probes answer, an account registered through REST lands as a row in Postgres,
server-written cache keys appear in Redis, the web container serves the bundle on
port 19992, and the media plane reports its own meters on its own port — then tears
everything down.

Tear down (add `-v` to also drop the Postgres and media volumes):

```sh
docker compose -f infra/compose/docker-compose.yml down
```

## Quick start — no containers

The server runs with in-memory backends by default, so a full stack is optional for
day-to-day work:

```sh
# terminal 1 — server on :8080, nothing to install first
cd server && MIGO_AUTH__TOKEN_KEY=development-only-insecure-token-key cargo run --bin migod

# terminal 2 — web on :19992
pnpm install
pnpm --filter "./packages/*" build
pnpm --filter @migo/web dev
```

In-memory data does not survive a restart. To develop against durable storage, set
`MIGO_STORE__BACKEND=postgres` / `MIGO_CACHE__BACKEND=redis` with their URLs, or copy
`config/migod.toml.example` to `config/migod.toml` and edit it there.

## The media plane is a second process

Section 92 keeps the SFU outside `migod`: it never touches plaintext media, and its
load profile is bandwidth rather than application logic. The stack above therefore
runs it as a service of its own, and the `sfu` configuration section is split
between the two:

| Key                 | `migod`                       | `sfu`                          |
| ------------------- | ----------------------------- | ------------------------------ |
| `sfu.public_url`    | set — it tells clients where  | set — the same value           |
| `sfu.ticket_key`    | set — it signs admissions     | set — the same value           |
| `sfu.bind`          | never set — it opens no media | set — the socket it listens on |
| `sfu.metrics_bind`  | never set                     | set, optionally                |

A join of three or more is answered with the address and a short-lived **ticket**: an
HMAC over this call, this account, this device and an expiry. The media plane checks
that ticket offline, so the two processes authenticate without sharing a store, a
session table, or a database read. Give them different keys and the plane admits
nobody; give `migod` a `sfu.bind` and it still opens nothing, because binding is the
media process's job.

The ticket key is the whole of the media plane's authentication, so its validation is
stricter than the rest of the file: it must decode to at least 32 bytes, and the
placeholder the repository documents — `development-only-insecure-sfu-ticket-key` — is
refused in every environment, development included. Unlike `auth.token_key`, neither
process can generate one for itself: the value is shared, so it has to come from the
operator.

## Building the images on their own

All three images take the **repository root** as their build context:

```sh
docker build -f infra/docker/Dockerfile.migod -t migo/migod   .
docker build -f infra/docker/Dockerfile.sfu   -t migo/migosfud .
docker build -f infra/docker/Dockerfile.web   -t migo/web     .
```

`migosfud` refuses to start without `MIGO_SFU__TICKET_KEY` (and the matching
`MIGO_SFU__PUBLIC_URL`); it opens one QUIC listener (`MIGO_SFU__BIND`, `udp/19443` by
default) and one scrape listener (`MIGO_SFU__METRICS_BIND`, `tcp/9090`), and its image
healthcheck is a scrape of the second, which is only bound after the first is.

The web client reads its server URLs at build time (Next.js inlines `NEXT_PUBLIC_*`
into the bundle), so point a non-local build at its server with build args:

```sh
docker build -f infra/docker/Dockerfile.web \
  --build-arg NEXT_PUBLIC_MIGO_API_URL=https://api.example.com \
  --build-arg NEXT_PUBLIC_MIGO_GATEWAY_URL=wss://api.example.com/ws \
  -t migo/web .
```

## Configuration

`migod` resolves configuration in this order, lowest to highest precedence:

1. built-in defaults
2. `config/migod.toml` (or the file named by `MIGO_CONFIG`)
3. environment variables — `MIGO_<SECTION>__<KEY>`, nesting with a double underscore
   (`MIGO_STORE__URL` sets `store.url`)
4. CLI flags

See `.env.example` for the full environment surface and `config/migod.toml.example`
for the file form. The keys the compose stack sets are documented inline in
`compose/docker-compose.yml`.

### Operational endpoints

| Path         | Meaning                                                        |
| ------------ | -------------------------------------------------------------- |
| `/health`    | liveness — the process can serve a request (image healthcheck) |
| `/ready`     | readiness — the node is willing to take traffic                |
| `/metrics`   | Prometheus exposition                                          |
| `/v1/config` | the node's public runtime configuration document               |

## Security

The compose stack and both example config files run in **development mode**. That mode
deliberately permits an ephemeral node key, the well-known token placeholder
`development-only-insecure-token-key`, a filesystem media backend, and the local
`migo:migo` database password. The server's own validation **refuses every one of these
outside development**, so this stack cannot be promoted to production by flipping
`MIGO_NODE__ENVIRONMENT` — it will refuse to start until each is replaced with a real
value. The media plane's ticket key is the one entry with no development exemption: the
placeholder is refused everywhere, and the value this stack ships is a local one that
belongs to no deployment. Generate real key material straight into the deploying process's environment
(`openssl rand -base64 32`); never commit it.
