# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- The amount of bytes read and written to a peer are now no longer reset after
  a reconnect (for outgoing connection).
- Renamed `connectionTxBytes` and `connectionRxBytes` on the peer stats struct
  to `txBytes` and `rxBytes` to better express that they are no longer tied to
  a single connection to the peer.

### Fixed

- When joining a link local multicast group on an interface returns a
  `Address already in use` error, the error is now ignored and the interface is
  considered to be joined.
- When sending an update to a peer, the source table is now correctly updated before
  the update is sent, instead of doing a batched source table update afterward.

## [0.4.3] - 2024-03-15

### Added

- Feature flag for message subsystem. It is enabled by default, but a user can
  make a custom build with `--default-features-false` which completely leaves out
  the message related code, should he desire this and have no need for it.
- Link local discovery now periodically checks for new IPv6 enabled interfaces
  and also joins the discovery multicast group on them.
- Trace logs are removed from release binaries at compile time, slightly reducing
  binary size.
- New `--silent` flag which disables all logging except error logs.

### Changed

- Update GitHub CI action to use latest version of the checkout action.
- Update GitHub CI action to stop using deprecated actions-rs actions.
- Failing to join the link local discovery multicast group now logs as warning
  instead of error.
- Failing to join any IPv6 multicast group for link local peer discovery will no
  longer disable local peer discovery entirely.

### Fixed

- Add proper validation when receiving an OOB ICMP packet.

## [0.4.2] - 2024-02-28

### Fixed

- Make sure the HTTP API doesn't shut down immediately after startup.

## [0.4.1] - 2024-02-27

### Added

- Admin API
  - Ability to see current peers and related info
  - Ability to add a new peer
  - Ability to remove an existing peer
  - List current selected routes
  - List current fallback routes
  - General node info (for now just the node subnet)

### Changed

- The tokio_unstable config flag is no longer used when building.
- The key file is now created without read permissions for the group/world.

### Removed

- .cargo/config.toml aarch64-linux target specific entries. Cross compilation for
  these platforms can use `cross` or entries in the global .cargo/config.toml of
  the developer instead.
- Sending SIGUSR1 to the process on unix based systems no longer dumps internal
  state, this can be accessed with the admin API instead.

## [0.4.0] - 2024-02-22

### Added

- Support for windows tunnels. While this works, there are no windows
  packages yet, so this is still a "developer experience".
- Validation on source IP when sending packets over TUN interface.

### Changed

- Overlay network is now hosted in 400::/7 instead of 200::/7.
- Key file is no longer created if it does not exist when the
  inspect command is run.
- Packets with destination outside the global subnet now return
  a proper ICMP instead of being silently dropped.

### Fixed

- Log the endpoint when a Quic connection can't be established.

## [0.3.2] - 2024-01-31

### Added

- If the router notices a Peer is dead, the connection is now forcibly terminated.
- Example Systemd file.

### Changed

- The multicast discovery group is now joined from all available
  interfaces. This should increase the resilience of local peer
  discovery.
- Setup of the node is now done completely in the library.
- Route selection now accounts for the link cost of peers when
  considering if it should switch to the new route.
- Hop count of data packets is now decremented on the first
  hop as well. As a result the sending node will show up in
  traceroute results.

### Fixed

- Inbound peers now replace existing peers in the peer manager. This should fix
  an issue where Quic could leave zombie connections.

## [0.3.1] - 2024-01-23

### Added

- You can now check the version of the current binary with the --version flag.
- Bandwidth usage is now tracked per peer.

### Changed

- Prefix decoding is now more resilient to bad prefix lengths.
- The `-k/--key-file` flag is now global, allowing you to specify it for (inspect)
  sub commands.
- Log the actual endpoint when we can't connect to it

## [0.3.0] - 2024-01-17

### Added

- Nodes can now explicitly request selected route(s) from connected peers by
  sending a Route Request Tlv.
- The router can now inform a Peer that the connection is seemingly
  dead. This should improve the reconnect speed on connection types
  which can't tell themselves if they died.

### Changed

- Locally discovered peers are now forgotten if we fail to connect to them 3
  times.
- Duration between periodic events has been increased, this should
  reduce bandwidth when idle to maintain the system.
- Address encoding in update packets is now in-line with address
  encoding as described by the babel RFC.

### Fixed

- TLV bodies of unknown type are now properly skipped. Previously, the
  calculation of the body size was off by one, causing the connection to
  the peer to always die. Now, these packets should be properly ignored.
- We are a bit more active now and no longer sleep for a second when we
  need to remove an expired route entry.

## [0.2.3] - 2024-01-04

### Added

- Added automatic release builds for aarch64-linux.

### Changed

- Reduce the Quic keep-alive timeout.

## [0.2.2] - 2024-01-03

### Changed

- Changed default multicast peer discovery port to 9650.

## [0.2.1] - 2024-01-03

### Added

- Experimental Quic transport. The same socket is used for sending and
  receiving. This transport is experimental and breaking changes are
  possible which won't be covered by semver guarantees for now.

## [0.2.0] - 2023-12-29

### Added

- Link local peer discovery over IPv6. The system will automatically detect peers on the same LAN and try to connect to them.
  This makes sure peers on the same network don't needlessly use bandwidth on external "hop" peers.
- Data packets now carry a Hop Limit field as part of the header. Every node decrements this value, and if it is decremented
  to zero, the packet is discarded
- Intermediate nodes can now send ICMP packets back to the source in
  reply to a dropped packet. This is useful if a hop does not have route
  to forward a packet, or the hop count for a packet reaches 0.
- Local node now returns an ICMP Destination unreachable - no route if a
  packet is sent on the TUN interface and there is no key for the remote
  address (so the user data can't be encrypted).
- Peers connected over IPv4 now incur a higher processing cost, causing
  IPv6 connections to be preferred for forwarding data.
- Peer addresses now include a protocol specifier, so multiple underlay
  connections can be specified in the future.

### Changed

- The peer manager now tracks sufficient info of each connected peer to
  avoid going through the router every time to see if it needs to
  reconnect to said peers.
- We don't send the receiver nodes IP address in IHU packets anymore, as the packet is sent over a unicast link.
  This is valid per the babel rfc.
- Setting the peers on the CLI now requires specifying the protocol to use.
  For now only TCP is supported.
- Change `--port` flag to `--tcp-listen-port` to more accurately reflect
  what it controls, as well as the fact that it is only for TCP.

### Removed

- Handshake when connecting to a new peer has been removed.

## [0.1.3] - 2023-11-22

### Added

- Add info log when next hop of a peer changes.
- Add windows builds to CI.

### Changed

- When printing the connected peers, print the underlay IP instead of the overlay IP.
- The link cost of a peer is now the smoothed average. This makes sure a single short latency spike doesn't disrupt routing.
- On Linux, set the TUN ip as /7 and avoid setting a /64 route. This brings it in line with OSX.
- When selecting the best route for a subnet, consider the currently
  installed route and only switch if it is significantly better, or
  directly connected.
- Increase the static link cost component of a peer. This will increase
  the value of a hop in the metric of a route, in turn increasing the
  impact of multiple hops on route selection. The route selection will
  thus be more inclined to find a path with fewer hops toward a
  destination. Ultimately, if multiple paths to a destination exist with
  roughly the same latency, they one with fewer hops should be
  preferred, since this avoids putting unnecessary pressure on multiple
  nodes in the network.
- IHU packets now include the underlay IP instead of the overlay IP.
- When a new peer connects the underlay IP is logged instead of the
  overlay IP.

### Fixed

- Ignore retraction updates if the route table has an existing but retracted route already. This fixes
  an issue where retracted routes would not be flushed from the routing table.

### Removed

- All uses of the exchanged overlay IP in the peer handshake are fully
  removed. Handshake is still performed to stay backwards compatible
  until the next breaking release.

## [0.1.2] - 2023-11-21

### Changed

- Allow routes with infinite metric for a subnet to be selected. They will only be selected if no feasible route
  with finite metric exists. They are also still ignored when looking up a route to a subnet.

### Fixed

- Don't trigger an update when a route retraction comes in for a subnet where the route is already retracted.
  This fixes a potential storm of retraction requests in the network.

## [0.1.1] - 2023-11-21

### Added

- CHANGELOG.md file to keep track of notable additions, changes, fixes, deprecations, and removals.
- A peer can now detect if it is no longer useable in most cases, allowing it to notify the router if it died. This
  allows near instant retraction of routes if a connection dies, decreasing the amount of time needed to find a
  suitable alternative route.
- Add CHANGELOG.md entries when creating a release.
- When sending SIGUSR1 to the process, the routing table dump will now include a list of the public key derived IP
  for every currently known subnet.
- You can now set the name of the TUN interface on the command line with the --tun-name flag.
- Added support for TUN devices on OSX.

### Changed

- When a peer is found to be dead, routes which use it as next-hop now have their metric set to infinity.
  If the route is selected, route selection for the subnet is run again and if needed a triggered update is sent.
  This will allow downstream peers to receive a timely update informing them of a potentially retracted route,
  instead of having to wait for route expiration.
- Account for the link with the peer of a route when performing route selection. This was not the case previously,
  and could theoretically lead to a case where a route was selected with a non-optimal path, because the lower metric
  was offset by a high link cost of the peer.

### Fixed

- Remove trailing 'e' character from release archive names

## \[0.1.0\] - 2023-11-15

### Added

- Initial working setup with end-to-end encryption of traffic.
- Ability to specify peers to connect to
- Message subsystem, including API to use it.
- CLI options to send and receive messages with the API.

[0.1.1]: https://github.com/threefoldtech/mycelium/compare/v0.1.0...v0.1.1
[0.1.2]: https://github.com/threefoldtech/mycelium/compare/v0.1.1...v0.1.2
[0.1.3]: https://github.com/threefoldtech/mycelium/compare/v0.1.2...v0.1.3
[0.2.0]: https://github.com/threefoldtech/mycelium/compare/v0.1.3...v0.2.0
[0.2.1]: https://github.com/threefoldtech/mycelium/compare/v0.2.0...v0.2.1
[0.2.2]: https://github.com/threefoldtech/mycelium/compare/v0.2.1...v0.2.2
[0.2.3]: https://github.com/threefoldtech/mycelium/compare/v0.2.2...v0.2.3
[0.3.0]: https://github.com/threefoldtech/mycelium/compare/v0.2.3...v0.3.0
[0.3.1]: https://github.com/threefoldtech/mycelium/compare/v0.3.0...v0.3.1
[0.3.2]: https://github.com/threefoldtech/mycelium/compare/v0.3.1...v0.3.2
[0.4.0]: https://github.com/threefoldtech/mycelium/compare/v0.3.2...v0.4.0
[0.4.1]: https://github.com/threefoldtech/mycelium/compare/v0.4.0...v0.4.1
[0.4.2]: https://github.com/threefoldtech/mycelium/compare/v0.4.1...v0.4.2
[0.4.3]: https://github.com/threefoldtech/mycelium/compare/v0.4.2...v0.4.3
[unreleased]: https://github.com/threefoldtech/mycelium/compare/v0.4.3...HEAD
