# Two-node local Migo stack

This directory hosts a self-contained 2-node setup for Migo, intended for
smoke tests of the realtime path and end-to-end chat over the gateway.
The two nodes are independent: each has its own `MIGO_NODE__ID`, its own
bind port, and its own PostgreSQL database. They share the Redis instance
on `localhost:16379` because the cache and rate-limiter keys are namespaced
by node id.

## Topology

```
+-------------------+         +-------------------+
|   migod-node-1   |         |   migod-node-2   |
|  MIGO_NODE__ID:   |         |  MIGO_NODE__ID:   |
|   node-1          |         |   node-2          |
|  HTTP  :18080     |         |  HTTP  :18081     |
|  WS    :18080/ws  |         |  WS    :18081/ws  |
+-------------------+         +-------------------+
        ^                                 ^
        |                                 |
        +----------------+----------------+
                         |
                  +-----------------+
                  |  tools/chatbot   |  <-- picks one node, registers
                  |  (TypeScript)    |      2 accounts, sends 10 msgs
                  +-----------------+
```

The chat bot in `tools/chatbot/` registers two accounts (`alice`, `bob`)
on whichever node `MIGO_API_URL` points at, opens a 1:1 conversation,
and sends ten round-trip messages so both accounts see every message
land. Each account subscribes to the conversation topic so inbound
frames are decrypted and printed.

This setup does not enable the federation mesh (`add_peer`). The two
nodes are independent — a client picks one, and the two accounts live
on the same node, so the message path is the single-node gateway path
the existing `migo-gateway` test suite already covers. Wiring the mesh
(allow-list, handshake, region routing) is a separate step that this
script does not run.

## What is in here

- `run.sh` — bring up node 1 and node 2, run the chat bot, and tear
  down on exit. Creates the two databases if they do not exist.
- `chatbot/` — the TypeScript chat bot package.

## Prerequisites

- A running PostgreSQL 16 or 17 on `localhost:15432`, user `migo`,
  password `migo` (the same one the repo's CI uses).
- A running Redis on `localhost:16379`.
- A built `migod` binary at `server/target/debug/migod`.

## Usage

```
./tools/2node/run.sh
```

The script prints each node's startup banner, waits for `/health` on
both, runs the chat bot against the first node, then SIGTERMs both
nodes. It does not run forever — the goal is a single end-to-end
smoke that you can read from start to finish.
