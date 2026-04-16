# mycelium_network — Project Notes for Claude

## Hero Socket Compliance

This project is a Hero-managed service and follows the Hero Unix socket strategy
(`/hero_sockets` skill) with **one documented exception**:

### TCP Ports Kept for Backwards Compatibility

The mycelium daemon intentionally retains two legacy TCP listeners for backwards
compatibility with existing clients, mobile apps, and third-party tooling:

| Server | Default address | Purpose |
|--------|----------------|---------|
| `mycelium_api::Http` | `127.0.0.1:8989` | HTTP REST API (`/api/v1/...`) |
| `mycelium_api::rpc::JsonRpc` | `127.0.0.1:8990` | TCP JSON-RPC 2.0 (jsonrpsee) |

**Do NOT remove these TCP servers.** They must remain alongside the Hero UDS socket.

### Canonical Hero Interface

New tooling (Hero shell, `mycelium.nu` client, `service_mycelium.nu`) uses the
Hero-compliant Unix socket exclusively:

```
$HERO_SOCKET_DIR/mycelium/rpc.sock   (default: ~/hero/var/sockets/mycelium/rpc.sock)
```

This socket serves HTTP/1.1 JSON-RPC 2.0 with all required Hero endpoints:
- `POST /rpc` — JSON-RPC 2.0 dispatch
- `GET  /openrpc.json` — OpenRPC spec
- `GET  /health` — health check
- `GET  /.well-known/heroservice.json` — discovery manifest

### CLI Subcommands

The `mycelium` CLI subcommands (peers, routes, proxy, stats) use the UDS socket.
The message subsystem (`send`/`recv`) still uses the TCP HTTP REST API (`--api-addr`).

## Sub-workspaces

This repo contains two independent Cargo workspaces:

| Directory | Binary | Purpose |
|-----------|--------|---------|
| `myceliumd/` | `mycelium` | Public network daemon |
| `myceliumd-private/` | `mycelium-private` | Private network daemon |

Build each separately — `cargo build` at the repo root will not work.

## Default Bootstrap Peers

Defined in `myceliumd-common/src/defaults.rs`. Used automatically when no peers
are configured via CLI or config file. Update this file to change the default
bootstrap peer list.
