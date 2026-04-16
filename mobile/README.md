# mobile crate

Embeddable bindings of the [Mycelium](../mycelium) overlay-network node, designed
to be loaded as a native library by mobile and desktop apps written in
**Dart/Flutter**, **Kotlin (Android)**, or **Swift (iOS / macOS)**.

The crate exposes a small, FFI-friendly Rust API that wraps a full Mycelium
node together with its HTTP and JSON-RPC management interfaces, and drives
them through a single command/response channel pair so the host app only ever
sees simple `String` / `Vec<String>` values across the language boundary.

## What it does

A host app links this crate (typically via `flutter_rust_bridge`,
`uniffi`, JNI, or a Swift Package wrapping the static lib) and uses it to:

- **Run a Mycelium node in-process** on the device, without spawning any
  external binary.
- **Use the OS-provided TUN file descriptor** (`tun_fd`) supplied by Android's
  `VpnService` / iOS `NEPacketTunnelProvider` / macOS `NetworkExtension`,
  instead of the node creating its own TUN interface (which is not permitted
  on mobile).
- **Connect to a configurable list of peers**, derive its IPv6 overlay address
  from a 32-byte private key generated on the device, and optionally serve the
  built-in HTTP API (`127.0.0.1:8989`) and JSON-RPC API (`127.0.0.1:8990`) for
  in-app dashboards.
- **Expose a SOCKS5 proxy discovery / selection layer** so the app can scan the
  overlay for SOCKS5 exit nodes, list them, connect to one, and disconnect.
- **Bridge platform logging** automatically: `logcat` on Android and
  `os_log` / Console.app on iOS and macOS, with sensible default log levels.

## Public API

All entry points are top-level `pub fn`s in `src/lib.rs`. Each blocking-looking
function is annotated with `#[tokio::main]` so the host can call them as plain
synchronous functions from any thread; internally they spin up a small Tokio
runtime, talk to the running node over an `mpsc` channel, and return.

| Function | Purpose |
| --- | --- |
| `start_mycelium(peers, tun_fd, priv_key, enable_dns, enable_api_server)` | Launches the node. Blocks for the lifetime of the node — call it from a dedicated thread. |
| `stop_mycelium()` | Sends a stop command and waits for the node loop to exit. |
| `get_peer_status()` | Returns one JSON string per peer with `protocol`, `address`, `peerType`, `connectionState`, `rxBytes`, `txBytes`, `discoveredSeconds`, `lastConnectedSeconds`. |
| `start_proxy_probe()` / `stop_proxy_probe()` | Starts/stops scanning the overlay for reachable SOCKS5 proxies. |
| `list_proxies()` | Returns the currently known proxy endpoints as `[ipv6]:1080` strings. |
| `proxy_connect(remote)` | Routes local SOCKS5 traffic through `remote` (or auto-pick when empty). |
| `proxy_disconnect()` | Tears down the active proxy route. |
| `generate_secret_key()` | Generates a fresh 32-byte node key. |
| `address_from_secret_key(key)` | Derives the Mycelium IPv6 address from a key. |

Every command/response function returns a `Vec<String>` whose **first element
is `"ok"` on success or an error string on failure**, so callers can use a
single uniform decoding path on the Dart/Kotlin/Swift side.

## Conventions

- **Default ports:** TCP underlay `9651`, QUIC underlay `9651`,
  peer-discovery `9650` (multicast discovery is **disabled** on mobile).
- **TUN handling:**
  - Android, iOS, and macOS-with-`mactunfd`: the host passes `tun_fd` and the
    node attaches to it.
  - Linux, Windows, plain macOS: the node creates its own `tun0` interface.
- **Channel timeout:** all command/response round-trips time out after
  2 seconds (`CHANNEL_TIMEOUT`) and surface as `NodeError::NodeTimeout`.
- **Single-node process:** the global `COMMAND_CHANNEL` / `RESPONSE_CHANNEL`
  statics mean only one node runs per process — matching the mobile model
  where a VPN service is a singleton.

## Cargo features

- `mactunfd` — on macOS, take a `tun_fd` from the host (NetworkExtension)
  instead of creating a `tun0` interface. Forwards to
  `mycelium/mactunfd`.

## Building for mobile targets

The crate produces a `cdylib`/`staticlib` consumable by mobile toolchains.
Typical target triples:

- Android: `aarch64-linux-android`, `armv7-linux-androideabi`,
  `x86_64-linux-android`
- iOS: `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `x86_64-apple-ios`
- macOS (with `--features mactunfd`): `aarch64-apple-darwin`,
  `x86_64-apple-darwin`

OpenSSL is vendored via the `mycelium/vendored-openssl` feature so no system
OpenSSL is required on the build host.
