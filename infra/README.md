# Migo infrastructure

Everything needed to build and run Migo — the `migod` server and the web client —
as containers, plus a local development stack that wires them to Postgres and Redis.

```
infra/
  docker/      image definitions (Dockerfile.migod, Dockerfile.web)
  compose/     the local development stack (docker-compose.yml)
  kubernetes/  reserved for cluster manifests
  terraform/   reserved for cloud provisioning
```

## Quick start — the full stack

From the repository root:

```sh
docker compose -f infra/compose/docker-compose.yml up --build
```

This builds both images and starts four services in dependency order:

| Service    | Purpose                                           | Address               |
| ---------- | ------------------------------------------------- | --------------------- |
| `postgres` | durable store; `migod` migrates it on boot        | internal              |
| `redis`    | cache and rate-limiter backend (no persistence)   | internal              |
| `migod`    | server: REST `/v1`, gateway `/ws`, probes at root | http://localhost:8080 |
| `web`      | the Next.js PWA                                   | http://localhost:3000 |

Open http://localhost:3000. Registration is enabled, so you can create an account
and sign in immediately.

Health checks gate the ordering: `migod` starts only once Postgres and Redis report
healthy, and `web` starts only once `migod` reports healthy. The first build compiles
the Rust workspace and can take several minutes; later builds reuse the cached
dependency layer.

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

# terminal 2 — web on :3000
pnpm install
pnpm --filter "./packages/*" build
pnpm --filter @migo/web dev
```

In-memory data does not survive a restart. To develop against durable storage, set
`MIGO_STORE__BACKEND=postgres` / `MIGO_CACHE__BACKEND=redis` with their URLs, or copy
`config/migod.toml.example` to `config/migod.toml` and edit it there.

## Building the images on their own

Both images take the **repository root** as their build context:

```sh
docker build -f infra/docker/Dockerfile.migod -t migo/migod .
docker build -f infra/docker/Dockerfile.web   -t migo/web   .
```

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

| Path       | Meaning                                                        |
| ---------- | -------------------------------------------------------------- |
| `/health`  | liveness — the process can serve a request (image healthcheck) |
| `/ready`   | readiness — the node is willing to take traffic                |
| `/metrics` | Prometheus exposition                                          |
| `/config`  | the effective runtime configuration document                   |

## Security

The compose stack and both example config files run in **development mode**. That mode
deliberately permits an ephemeral node key, the well-known token placeholder
`development-only-insecure-token-key`, a filesystem media backend, and the local
`migo:migo` database password. The server's own validation **refuses every one of these
outside development**, so this stack cannot be promoted to production by flipping
`MIGO_NODE__ENVIRONMENT` — it will refuse to start until each is replaced with a real
value. Generate real key material straight into the deploying process's environment
(`openssl rand -base64 32`); never commit it.
