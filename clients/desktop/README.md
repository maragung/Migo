# Migo desktop client

A native end-to-end encrypted messenger in Rust, drawn with [egui](https://github.com/emilk/egui)
through [eframe](https://docs.rs/eframe). One binary, `migo-desktop`, with no runtime beyond a GL
3.3 driver and a window system.

It talks to the same two endpoints every other Migo client talks to — the REST API for account and
key operations, the gateway WebSocket for everything realtime — and it performs the same
cryptography, because it links the same crates the server does: `migo-core`, `migo-wire`,
`migo-protocol` and `migo-crypto`. There is no desktop-specific protocol and no desktop-specific
crypto. That is the point of depending on those four by path rather than reimplementing them.

## Building

```sh
# From the repository root: type-check and lint, no linking, no graphics headers needed.
make check-desktop
make lint-desktop
make fmt-check-desktop

# A real build, from this directory.
cargo build --release
```

`cargo check` and `cargo clippy` work on a headless container with no graphics development
packages installed, because nothing is linked: winit and glutin open `libGL`, `libX11`,
`libxkbcommon` and `libwayland` at run time. A `cargo build` does link, and needs the development
headers:

```sh
sudo apt-get install -y libx11-dev libxcursor-dev libxi-dev libxrandr-dev \
    libxkbcommon-dev libwayland-dev libgl1-mesa-dev libegl1-mesa-dev
```

Release binaries are produced by GitHub Actions rather than by hand — see
`.github/workflows/release.yml`, which publishes
`client_desktop-<version>-x86_64-unknown-linux-gnu.tar.gz` with `migo-desktop` inside it. The CI
job installs the packages above; a developer who only ever runs `make check-desktop` does not have
to.

## Running

```sh
cargo run --release
```

The first screen asks for a server address, an account name and a passphrase. There is no
"remember me": the passphrase is the only thing that can open the vault, and this process
deliberately forgets it the moment the vault is unlocked.

`RUST_LOG` controls logging, e.g. `RUST_LOG=migo_desktop=debug`. Nothing that log can emit is
sensitive by construction — see the rules in `src/net/mod.rs` — but it is off by default anyway.

## Where things live

| Path           | What it owns                                                                 |
| -------------- | ---------------------------------------------------------------------------- |
| `src/main.rs`  | Process entry: logging, the vault path, `eframe::NativeOptions`.             |
| `src/app.rs`   | The `eframe::App` implementation. One frame, and the drain of worker events. |
| `src/model.rs` | Plain UI-facing data: accounts, conversations, messages, toasts. No I/O.     |
| `src/theme.rs` | The palette, type scale, spacing and radii. Light and dark.                  |
| `src/ui/`      | Screens (`auth`, `chat`) and the shared widget vocabulary.                   |
| `src/net/`     | The worker: gateway WebSocket, REST client, reconnect schedule.              |
| `src/crypto/`  | Session policy, the section-11 envelope, the inner content codec.            |
| `src/vault.rs` | The sealed on-disk key store.                                                |

## Two threads, two channels, and no locks between them

egui repaints by calling one function per frame. A frame function that awaited a socket would stop
repainting for as long as the await lasted, and the window would be visibly frozen — not slow,
frozen. A mutex that the paint loop has to acquire is the same freeze arriving by a longer route.

So the split is absolute: **the UI thread never touches a socket, a database or a ratchet.** All of
that lives in one Tokio worker, and the two sides exchange values over two channels:

- commands out, on a `tokio::sync::mpsc::unbounded_channel` — the UI sends and never blocks;
- events in, on a `std::sync::mpsc` channel — the UI drains it with `try_recv()` at the top of each
  frame and never blocks there either.

The worker calls `ctx.request_repaint()` when it has something new, which is what wakes an idle
window. Nothing else in the program requests an unconditional repaint except the toast animation,
which is the one thing that changes without input.

Screens do not hold a handle to the worker. Each is handed a `ui::Context` carrying read-only facts
plus `commands: &mut Vec<Command>` and `navigate: &mut Option<Screen>`; `app.rs` drains both _after_
the frame is done. A screen therefore cannot send a command in the middle of laying itself out, and
cannot change which screen is being drawn while it is being drawn.

## The vault

Keys live in one file, sealed with a key derived from the passphrase by Argon2id at 64 MiB, 3
passes, 1 lane. Format `MIGOVLT1`:

```
magic(8) || version(1) || salt(16) || memory_kib LE(4) || time_cost LE(4) || lanes LE(4) || sealed body
```

The whole 37-byte header is the AEAD associated data, so the cost parameters cannot be edited down
by anyone who can write the file. Writes are atomic (temp file, then rename) and the file is
`0o600` on unix.

The passphrase is not retained after unlock. That has one visible consequence: the pool of
one-time prekeys can be watched but not refilled, because minting more means sealing them, which
needs the passphrase. The client warns when the pool drops below a fifth and asks the user to sign
in again — which is honest about what it can and cannot do on its own.

## Sessions

One Double Ratchet per remote **device**, not per user and not per conversation. X3DH runs once per
session, on the first send; the prekey preamble then rides on every message until the peer replies,
because until a reply arrives there is no evidence the peer processed it, and dropping it early
strands the session.

The ratchet advances only on a successful decryption. A message that fails to decrypt leaves the
session exactly as it was — otherwise one forged frame would be a denial of service against a real
conversation.

## Reconnecting

Exponential backoff from 500 ms to a 30 s cap, up to eight attempts, with up to half a second of
jitter per attempt so a fleet of clients does not return in lockstep and knock a recovering node
over again.

The waiting is one arm of the worker's `select!`, not a sleep inside the failure handler. That
placement is deliberate: a sleep in the handler would leave the command channel unserviced for the
whole backoff, so a user who closed the window during an outage would wait up to thirty seconds for
the process to notice. It also keeps the reconnect path from being re-entrant, since the handler no
longer calls the thing that can call the handler.
