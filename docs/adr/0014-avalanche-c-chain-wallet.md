# ADR-0014 — Avalanche C-Chain for the EVM wallet's first on-chain release

- **Status:** Accepted · **Date:** 2026-09-01 · **Brief refs:** §184, migo-update-1.md #40-#45

## Context

ADR-0013 shipped the EVM wallet as address display and registration only, explicitly
deferring balances, RPC, and transaction signing. The user has now chosen the network
for that deferred half: **Avalanche C-Chain**. The wallet keys already exist — BIP-32/44
`m/44'/60'/0'/0/i` from the root's `MIGO/EVM/V1` domain — and an EVM address is the same
20 bytes on every EVM chain, so this decision touches no server schema and no key
derivation. What it decides is which chain the first send flow targets, which RPC the
clients pin, and what "confirmed" means.

## Options

| Option                   | Pros                                                                                         | Cons                                                      |
| ------------------------ | -------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| Ethereum mainnet         | Largest ecosystem, most tooling                                                              | Highest fees for a first send flow; nothing Migo-specific |
| Avalanche C-Chain        | EVM-identical, sub-second finality, low fees, public RPC, Fuji testnet for free verification | Smaller ecosystem than Ethereum; AVAX not ETH             |
| L2 (Arbitrum/Base)       | Cheap                                                                                        | Adds sequencer trust story; chain-specific gotchas        |
| Multi-chain from day one | No lock-in                                                                                   | Doubles the test surface for the riskiest new feature     |

## Decision

Avalanche C-Chain is the first network: mainnet chain id **43114**
(`https://api.avax.network/ext/bc/C/rpc`) and Fuji testnet chain id **43113**
(`https://api.avax-test.network/ext/bc/C/rpc`). Clients call the RPC directly from a
pinned endpoint per network — the Migo server never proxies blockchain traffic and
never learns a transaction exists. Transactions are **EIP-1559 (type 0x02) only**,
signed locally by the secp256k1 key derived from the root, with RLP and the EIP-712
typed-data hash implemented in `migo-account` and ported to TypeScript and Kotlin
under the same cross-language vectors as every other account transform. Every RPC
session verifies `eth_chainId` against the configured network before a transaction is
built, and "RPC accepted" (PENDING) is never the same state as "chain confirmed"
(CONFIRMED requires a receipt with status 1) — the §41 state machine is enforced in
all three clients. Activity is the locally tracked transaction list, persisted as an
encrypted vault field, not a blockchain index.

## Consequences

AVAX is the asset users will send, so estimates, fees, and balances are denominated in
AVAX/nAVAX, and a first-mainnet-send warning is part of the UI contract. Fuji is the
verification network: the release checklist runs a real Fuji transaction, which needs a
funded test key. Native AVAX transfer is this release's whole on-chain surface —
ERC-20/ARC-20 sends, approvals, and swap remain deferred, and the UI must not imply
they exist. Plaintext private-key export stays unimplemented (the `.migo` container is
the only export door), and no wallet-API surface for dApps ships until it can carry
per-operation user approval. Avalanche's EVM equivalence means a second network later
is a configuration entry and a new vector case, not a port.
