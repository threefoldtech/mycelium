# Mycelium crate overview

The repository is a multi-crate Cargo workspace. This document explains what
each crate does, how they fit together, and how the three "similar-looking"
names — **`mycelium`**, **`myceliumd`**, and **`mycelium-cli`** — are actually
quite different.

## TL;DR — the three names

| Name | What it is | Output | Who uses it |
| --- | --- | --- | --- |
| **`mycelium`** | The **library**: routing engine, crypto, TUN, message stack | `rlib` (no binary) | every other crate in this repo, plus the mobile bindings |
| **`mycelium-cli`** | A library of **CLI command handlers** that talk to a running node over its HTTP API | `rlib` (no binary) | `myceliumd` and `myceliumd-private` (linked in via `myceliumd-common`) |
| **`myceliumd`** | A thin **`main.rs`** that wires `myceliumd-common` into a daemon binary | binary `mycelium` | end users who want to join the public Mycelium network |

So:
- `mycelium` ≈ "the engine" (no `main`, no CLI parsing).
- `mycelium-cli` ≈ "what `mycelium peers list` actually does" (no `main`, no daemon).
- `myceliumd` ≈ "the actual program you run".

The on-disk binary is called `mycelium` (renamed from `myceliumd`) for
historical and UX reasons.

## Workspace layout

The workspace splits roughly into three layers:

```
            +-----------------------------+
            |        binaries             |
            |  myceliumd, myceliumd-      |
            |  private, mobile (cdylib)   |
            +--------------+--------------+
                           |
            +--------------v--------------+
            |     daemon glue layer       |
            |   myceliumd-common          |
            |  (config, CLI parsing,      |
            |   logging, run_node)        |
            +--+---------+---------+------+
               |         |         |
        +------v-+ +-----v----+ +--v-----------+
        | mycel- | | mycel-   | | mycel-       |
        | ium    | | ium-api  | | ium-cli      |
        | (core) | | (HTTP +  | | (HTTP client |
        |        | |  JSON-   | |  command     |
        |        | |  RPC)    | |  handlers)   |
        +---+----+ +----+-----+ +------+-------+
            |           |              |
            +---+   +---+              |
                |   |                  |
        +-------v---v-------+    (uses mycelium-api
        |  mycelium         |     types over the wire)
        |  (the engine)     |
        +-------+-----------+
                |
        +-------v-----------+
        | mycelium-metrics  |
        | (Prometheus /     |
        |  noop adapters)   |
        +-------------------+
```

## Per-crate detail

### `mycelium/` — the engine library

- **Type:** library (`rlib`), `package.name = "mycelium"`.
- **Workspace status:** in the root `[workspace] members`.
- **Has `main`?** No.

Implements every part of a Mycelium overlay node: Babel-style routing
(`router.rs`, `babel/`, `routing_table/`), per-peer connection management
(`connection/`, `peer.rs`, `peer_manager.rs`), the underlay endpoints
(TCP, QUIC, optional vsock — `endpoint.rs`), x25519 + AES-GCM crypto
(`crypto.rs`), packet/data plane (`packet/`, `data.rs`), TUN integration
per OS (`tun/`), the SOCKS5 overlay proxy (`proxy.rs`), the optional
end-to-end message stack (`message/`, behind the `message` feature), DNS
forwarding (`dns.rs`), and a CDN cache (`cdn.rs`).

The public entry point is the `Node<M: Metrics>` type in `lib.rs` — every
binary builds and drives a single `Node`.

Cargo features:
- `message` — enable the message subsystem (`Node::push_message`,
  `pop_message`, etc.).
- `private-network` — enable TLS-secured private overlays (pulls in
  `openssl` / `tokio-openssl`).
- `vendored-openssl` — statically link OpenSSL.
- `mactunfd` — on macOS, accept an externally-provided TUN file
  descriptor (used by the App Store / NetworkExtension build path).

### `mycelium-api/` — HTTP and JSON-RPC server

- **Type:** library (`rlib`).
- **Workspace status:** in `[workspace] members`.
- **Has `main`?** No.

Wraps a running `mycelium::Node` and exposes it over two interfaces:

1. An **axum HTTP API** (`Http::spawn`, default `127.0.0.1:8989`,
   `lib.rs` and `message.rs`).
2. A **JSON-RPC server** (`JsonRpc::spawn`, default `127.0.0.1:8990`,
   `rpc.rs` + `rpc/`) built on `jsonrpsee`. The OpenRPC spec is
   `include_str!`'d from `docs/openrpc.json` and served by the
   standard `rpc.discover` method.

Cargo features:
- `message` — also expose the message endpoints (mirrors the
  `mycelium/message` feature).

### `mycelium-cli/` — CLI command handlers (library)

- **Type:** library (`rlib`).
- **Workspace status:** in `[workspace] members`.
- **Has `main`?** No.

Despite the `-cli` suffix this crate is **not a binary**. It is a
collection of functions like `list_peers`, `add_peers`, `send_msg`,
`recv_msg`, `connect_proxy`, `list_packet_stats`, etc. (`src/lib.rs`),
each of which uses `reqwest` to call the running daemon's HTTP API and
prints results with `prettytable-rs` or as JSON.

In other words: it implements the *behavior* of the `mycelium peers list`,
`mycelium message send`, ... subcommands, but the actual `clap` definitions
and `main()` live elsewhere (`myceliumd-common`, `myceliumd`,
`myceliumd-private`).

Cargo features:
- `message` — include `send_msg`/`recv_msg` and pull in the message
  feature on `mycelium` and `mycelium-api`.

### `mycelium-metrics/` — metrics adapters

- **Type:** library (`rlib`).
- **Workspace status:** in `[workspace] members`.
- **Has `main`?** No.

Implements the `mycelium::metrics::Metrics` trait twice:
- `NoMetrics` — does nothing (always available).
- `PrometheusExporter` — counts events and serves `/metrics` via axum
  (behind the `prometheus` feature).

The `Node` is generic over `M: Metrics`, so the binary chooses one of
these (or its own implementation) at construction time.

### `myceliumd-common/` — daemon glue

- **Type:** library (`rlib`).
- **Workspace status:** **excluded** from the root workspace
  (`exclude = ["myceliumd", "myceliumd-private", "myceliumd-common", "mobile"]`).
- **Has `main`?** No.

The single `src/lib.rs` (~1000 lines) holds everything that is shared
between the public daemon and the private daemon:

- The `clap` `Cli<N>`, `Command`, `MessageCommand`, `PeersCommand`,
  `RoutesCommand`, `ProxyCommand`, `StatsCommand` definitions — i.e.
  the entire user-facing command tree.
- TOML config-file loading and CLI/file merging (`MyceliumConfig`,
  `merge_config`, `load_config_file`).
- Logging setup (`init_logging`, `LoggingFormat`).
- Key file handling (`resolve_key_path`, `load_key_file`).
- `run_node(...)` — actually constructs a `mycelium::Node`, spawns the
  HTTP and JSON-RPC servers from `mycelium-api`, and the Prometheus
  exporter from `mycelium-metrics`.
- `dispatch_subcommand(...)` — when the user runs a CLI subcommand like
  `mycelium peers list`, this calls into `mycelium-cli` with the right
  HTTP endpoint.

This crate exists so that **`myceliumd` and `myceliumd-private` can each
be a ~30-line `main.rs`** that just selects which `NodeArguments` flavour
to use.

### `myceliumd/` — the public-network daemon binary

- **Type:** binary (`bin` named `mycelium`).
- **Workspace status:** **excluded** from the root workspace.
- **Has `main`?** Yes — 30 lines (`src/main.rs`).

The default daemon: depends only on `myceliumd-common` and `clap`. Calls
`Cli::<NodeArguments>` (the plain node arguments — no private-network
flags) and either `run_node` or `dispatch_subcommand`. This is the
binary published as `mycelium` on the GitHub Releases page and intended
for joining the public Mycelium overlay.

### `myceliumd-private/` — the private-network daemon binary

- **Type:** binary (`bin` named `mycelium-private`).
- **Workspace status:** **excluded** from the root workspace.
- **Has `main`?** Yes — ~80 lines (`src/main.rs`).

Same shape as `myceliumd` but its `Cli` parameter is `PrivateNodeArguments`,
which adds `--network-name` and `--network-key-file`. Depends on
`mycelium` with the `private-network` feature (TLS-secured peer
connections via `openssl`). Has a `vendored-openssl` feature to
statically link OpenSSL.

Can technically still join the public network today, but the README
recommends using `myceliumd` for that.

### `mobile/` — Dart/Flutter/Kotlin/Swift bindings

- **Type:** library, intended to be built as `cdylib`/`staticlib`.
- **Workspace status:** **excluded** from the root workspace.
- **Has `main`?** No, but exposes FFI-friendly `pub fn`s.

Embeds `mycelium` (with `vendored-openssl`) and `mycelium-api` directly
in-process for mobile and macOS apps using the OS-provided TUN file
descriptor. See `mobile/README.md` for the API surface
(`start_mycelium`, `stop_mycelium`, `get_peer_status`, the proxy
commands, key generation, etc.).

## Why `myceliumd*` and `mobile` are excluded from the workspace

The root `Cargo.toml` only includes the four "always buildable, always
public-network" crates:

```toml
members = ["mycelium", "mycelium-metrics", "mycelium-api", "mycelium-cli"]
exclude = ["myceliumd", "myceliumd-private", "myceliumd-common", "mobile"]
```

The excluded crates each have their own `Cargo.lock` and their own
target-specific quirks:

- `myceliumd-private` requires OpenSSL and pulls in the
  `private-network` feature on `mycelium` — not everyone wants that
  build dependency.
- `mobile` is built for Android/iOS/macOS and uses
  `vendored-openssl`.
- `myceliumd` and `myceliumd-private` both depend on `myceliumd-common`,
  which depends on `mycelium-cli`/`-api`/`-metrics` with specific
  feature sets — keeping them out of the workspace lets each one pin
  features independently.

A plain `cargo build` at the repo root therefore builds the library
crates only. To get a runnable daemon, `cd myceliumd && cargo build`.

## How a request flows through the crates

A user runs `mycelium peers list`:

1. `myceliumd/src/main.rs` parses argv with `Cli::<NodeArguments>` from
   `myceliumd-common`.
2. The `Peers { command: List { json } }` subcommand is matched, so
   `dispatch_subcommand` is called instead of `run_node`.
3. `myceliumd-common::dispatch_subcommand` calls
   `mycelium_cli::list_peers(api_addr, json)`.
4. `mycelium-cli` does an HTTP GET against the running daemon's
   `mycelium-api` HTTP server (`/api/v1/admin/peers`).
5. `mycelium-api`'s axum handler calls `node.peer_info()` on the
   `mycelium::Node` it holds.
6. The list is serialized back to JSON, `mycelium-cli` formats it as a
   table or raw JSON, and the user sees the output.

A user pushing a message via JSON-RPC instead of the HTTP API hits
`mycelium-api/src/rpc.rs`'s `pushMessage`, which calls
`node.push_message(...)` on the same `Node` — exactly the same engine,
different surface.

## Summary table

| Crate | Kind | In root workspace? | Main entry | Depends on |
| --- | --- | --- | --- | --- |
| `mycelium` | library | yes | — | (leaf) + `mycelium-cdn-registry` (git) |
| `mycelium-api` | library | yes | — | `mycelium`, `mycelium-metrics` |
| `mycelium-cli` | library | yes | — | `mycelium`, `mycelium-api` |
| `mycelium-metrics` | library | yes | — | `mycelium` |
| `myceliumd-common` | library | no (excluded) | — | `mycelium`, `mycelium-api`, `mycelium-cli`, `mycelium-metrics` |
| `myceliumd` | binary `mycelium` | no (excluded) | `src/main.rs` | `myceliumd-common` |
| `myceliumd-private` | binary `mycelium-private` | no (excluded) | `src/main.rs` | `mycelium` (with `private-network`), `myceliumd-common` |
| `mobile` | cdylib/staticlib | no (excluded) | FFI `pub fn`s | `mycelium`, `mycelium-api` |
