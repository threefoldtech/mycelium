# SOCKS5 proxy

Mycelium can forward a local TCP port to a remote SOCKS5 server running on another node in the overlay. This lets applications on your machine use a local SOCKS5 endpoint while all traffic to the SOCKS server is transparently tunneled over the Mycelium network to a selected peer.

High level:
- Discovery: the node periodically probes known peers for an open SOCKS5 service on port 1080.
- Connect: you can connect to the “best” discovered proxy automatically, or to a specific remote address.
- Forwarding: once connected, the node binds a local listener on [::]:1080 and forwards all connections bidirectionally to the chosen remote SOCKS5 service.
- Disconnect: stop forwarding and clear the target.

## How it works

- Discovery and probe cycle
  - Interval: every 60s the node scans the currently selected routes and attempts a TCP connection to each peer’s address on port 1080.
  - Probe handshake: it sends a SOCKS5 client greeting [0x05, 0x01, 0x00] and expects the server choice [0x05, 0x00] (no authentication).
  - Classification:
    - Valid: responds with version 5 and method 0x00 (no auth).
    - AuthenticationRequired: responds with 0xFF (no acceptable methods). These proxies are discovered but not auto-selected.
    - NotAProxy / NotListening: anything else or no response within 5s.
  - The discovery status is cached per IPv6 address.

- Selecting and connecting
  - Auto-select: if you call “connect” without a remote, Mycelium races all “Valid” proxies and picks the one that successfully responds first to the SOCKS5 greeting.
  - Explicit remote: you can provide a specific socket address (e.g. [407:…]:1080). In this case the system does not probe or validate it first; it will forward to that destination directly.

- Local listener and forwarding
  - Bind: after connecting, the node spawns a TCP listener on [::]:1080.
  - Behavior: each inbound connection is proxied bidirectionally to the chosen remote (“copy_bidirectional”). The local node itself does not implement the SOCKS protocol; it merely forwards bytes to the remote SOCKS5 server. From the application perspective, talking to localhost:1080 works because the real SOCKS server is at the remote node.
  - Lifetime: all active forwarded streams are cancelled when you disconnect or when the node is asked to stop.

- Disconnect
  - Cancels the proxy listener and all active streams, and clears the selected remote target.

## Using the local SOCKS endpoint

Once connected, point your applications to the local listener:
- Host: 127.0.0.1 or ::1
- Port: 1080
- Protocol: SOCKS5

Because the local listener just forwards bytes to the remote SOCKS server:
- If the remote SOCKS server requires authentication, your application must authenticate; the local node does not terminate SOCKS.
- Auto-selection only considers “no-authentication” servers; to use an auth-required proxy, connect explicitly with the remote address.

## Security considerations

- Binding to all interfaces: the listener binds to [::]:1080 (all interfaces). This can expose the SOCKS endpoint to your local network. Use OS firewalling to restrict access to localhost only if desired.
- Trust model: SOCKS5 is terminated on the remote node you connect to. Ensure you trust that node and understand the network path from it to the ultimate destinations.
- Discovery bandwidth: probes are lightweight (short handshake) and run every 60s. You can stop probing to reduce background activity.

## Limitations and notes

- Fixed local port: the listener uses port 1080 and cannot be changed via the current API. If another process occupies 1080, the listener fails to start.
- IPv6 bind: binding is performed on IPv6 “::”. Whether this socket also accepts IPv4-mapped connections depends on OS configuration; connecting to [::1]:1080 (or 127.0.0.1:1080 on systems that map v4) is recommended.
- Auto-selection filters: only “Valid” (no-auth) proxies are candidates for auto-selection. “AuthenticationRequired” proxies are discovered and listed, but not auto-picked.
- Forwarding only: the node does not implement SOCKS5 locally; it forwards your application’s SOCKS conversation to the remote SOCKS server.

## Troubleshooting

- “Could not bind TCP listener” on port 1080
  - Another process is using the port. Stop that process or reconfigure your environment. The local port is fixed in this version.

- “No valid proxy available or connection failed”
  - The network has no discovered “Valid” proxies. Start probing, verify connectivity to peers, or connect to a specific known remote address explicitly.

- Local apps cannot reach the listener
  - Ensure you are connecting to ::1:1080 or 127.0.0.1:1080, and that firewall rules allow local connections. Check logs to confirm that the listener started successfully.

- Authentication-required remote
  - Auto-select won’t pick it. Use explicit connect with its socket address and configure your app to authenticate over SOCKS as needed.
