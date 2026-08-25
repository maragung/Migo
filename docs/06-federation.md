# 06 — Multi-region & server mesh

"P2P" in Migo means **server-to-server**, never client-to-client for normal chat
(brief §4). Client P2P would leak user IPs, defeat moderation, and add NAT traversal and
reliability problems in exchange for nothing we need.

## 1. Topology

```
   asia-se-1 ──── asia-ne-1            eu-central-1 ──── eu-west-1
       │  ╲          │                      │   ╲            │
       │   ╲─────────┼──────────────────────┘    ╲───────────┤
       │             │                                       │
   us-east-1 ─────── us-west-1 ──────────────────────────────┘
```

A partial mesh with per-region peering plus a small number of cross-region links.
Full mesh at 40 nodes is 780 links; we peer regions, not nodes, and elect **relays**
per region for cross-region traffic.

## 2. Node identity

Every node holds an Ed25519 keypair and publishes `node_id`, `region`, `country`,
`capabilities`, `software_version`, `protocol_version`, `health` (brief §6). The private
key is injected from the secret manager, never in the image, never in Git.

## 3. Mesh handshake

```
A → B : MESH_HELLO   { node_id, region, protocol, capabilities, nonce_a, timestamp }
B → A : MESH_CHALLENGE { nonce_b, sig_b = Sign(b, "migo-mesh-v1" ‖ nonce_a ‖ nonce_b ‖ id_a) }
A → B : MESH_PROOF     { sig_a = Sign(a, "migo-mesh-v1" ‖ nonce_b ‖ nonce_a ‖ id_b) }
B → A : MESH_WELCOME   { session_id, routing_epoch, peers[] }
```

Properties, each closing a specific hole:

- **Mutual** authentication — both sides prove possession.
- Both nonces **and** the peer's id are inside every signature, so a signature cannot be
  replayed against a different peer or in the opposite direction.
- A domain-separation string prevents cross-protocol signature reuse.
- Timestamp skew beyond ±60 s is rejected.
- Sequence numbers on the session give replay protection afterwards (brief §7).
- Nodes must be on the allow-list: **no anonymous node joins production**.
- Key rotation is an overlap window with both keys accepted, then revocation.

## 4. Routing

Each node keeps a routing table `(user_id | room_id) → home_node`, versioned by a
`routing_epoch`, gossiped as **deltas** on change, with periodic full reconciliation.
Lookup miss → ask the region relay → cache negatively with a short TTL.

Cross-region private message:

```
A (Jakarta) → asia-se-1 → [mesh, ciphertext] → eu-central-1 → B (Berlin)
```

The payload stays sealed. Gateways route metadata only and never decrypt private content
(brief §53).

## 5. Room home region and sharding

A room has a **home region** that owns sequencing. Remote regions run an **edge shard**
that subscribes once to the home region and fans out locally (brief §54–55):

```
room MGO-ROOM-82F91A (home: asia-se-1)
  ├─ shard asia-se-1   : 12 000 subscribers   ← authoritative sequencer
  ├─ edge eu-central-1 :  4 000 subscribers   ← one mesh stream in, local fanout out
  └─ edge us-east-1    :  6 000 subscribers
```

A message crosses each region link **once**, not once per subscriber. That is the
difference between a 50 000-member room being routine and being an incident.

## 6. Client region selection (brief §5)

On login the client scores candidate gateways on RTT (median of 3 probes), a health hint
from the discovery endpoint, and last-known stability, then pins its choice for the
session. Failover order: same-region alternate → nearest region → any healthy region.
Pinning matters: flapping between gateways costs a handshake and a resync each time.

## 7. Health and ejection

States: `Healthy`, `Warning`, `Degraded`, `Offline`, `Maintenance` (brief §64). Peers
sample each other and the gateway discovery endpoint removes non-`Healthy` nodes from
routing automatically. Ejection is fast (seconds); re-admission is slow and requires
passing health checks (minutes) — the asymmetry prevents a flapping node from oscillating
the whole cluster.

## 8. Join / leave

Join (brief §100): generate or load identity → register → authenticate → fetch config →
self health-check → join mesh → sync routing → **then** accept traffic.
Leave (brief §101): stop accepting new connections → `RECONNECT_HINT` to clients with
jittered deadlines → finish in-flight work → flush outbox → transfer room ownership →
notify peers → close.

## 9. Partition behaviour

Both sides keep serving local users. Cross-region traffic queues in the outbox with a
bounded age. On heal, the outbox drains and per-message dedup makes double delivery
harmless. Room ownership is **never** taken over unilaterally during a partition: two
sequencers for one room would corrupt ordering. A room whose home region is unreachable
goes read-only from edge cache, and says so in the UI.

## 10. What is _not_ federated

Private E2E plaintext (does not exist server-side), password hashes and credentials
(home region only), and node private keys. The mesh moves ciphertext, routing metadata,
room events, and presence summaries — nothing else.
