# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.0] - 2025-12-08

### Added

- Optional DNS resolver. When enabled this will bind UDP port 53 on the system.
  For now, this uses the system configured resolvers to resolve queries. In the
  future, this will be expanded to redirect queries for certain TLD's to alternative
  backend.

### Changed

- The Quic connection type now uses quic datagrams to transport __data__ (packets
  coming from the TUN device) to the peer. Protocol traffic is still sent over a
  bidirectional Quic stream (which supports retransmits).

### Fixed

- Return actuall amount of bytes sent to peers instead of the amount of bytes received
  from them.
- Improve handling of completely local packets on MacOS. This will allow the kernel
  to reply to ping packets send from the local system to the TUN interface, among
  other things.
- Fixed a potential system lock when sending messages to a (recently) offline receiver.

## [0.6.2] - 2025-09-19

**IMPORTANT**

This release changes the default location of the private key used to derive the
local IP address. If you upgrade to this version and want to keep your IP/subnet,
and don't set the `-k/--key-file` flag, move your key file to the new default
location or add the flag pointing to your existing key file.

### Added

- New log format option `plain`, this option is the same as logfmt, but with colors
  always disabled.
- Added auto discovery of Socks5 proxies on the overlay, and the ability to proxy
  local Socks5 connections to a chosen (manual or automatic) remote.
- New `generate-keys` subcommand which generates the key file without running a
  daemon. It can also be used to generate fresh keys, should that be needed.

### Changed

- Default key path (which is used if the `--key-file` flag isn't set) is changed
  to a fixed path on the system in application data, instead of the old local file.

### Fixed

- The RPC API now returns an empty result instead of an error when popMessage does
  not have any message to return within the specified timeout.

## [0.6.1] - 2025-05-14

### Added

- When a route is used which is about to expire, we now send a route request to
  try and refresh its duration before it expires.
- We now track when a peer was fist discovered and when we last connected to it.
  This info is displayed in the CLI when listing peers.
- We now maintain a cache of recently sent route requests, so we can avoid spamming
  peers with duplicate requests.

### Changed

- Only keep a record of retracted routes for 6 seconds instead of 60. We'll track
  how this affects the route propagation before removing this altogether.

### Fixed

- Fixed an unsoundness issue in the routing table clone implementation.
- Clear dead peer buffer once peers have been removed from the routing table.
- Properly reply with an address unreachable ICMP when pinging an IP in the local
  subnet which does not exist.
- Verify a packet has sufficient TTL to be routed before injecting it, and reply
  with a TTL exceeded otherwise. This fixes an issue where packets with a TTL of
  1 and 0 originating locally would not result in a proper ICMP reply. This happens
  for instance when using `traceroute`.
- Check the local seqno request cache before sending a seqno request to a peer,
  to avoid spamming in certain occasions.
- Don't accept packet for a destination if we only have fallback routes for said
  destination.

## [0.6.0] - 2025-04-25

This is a breaking change, check the main README file for update info.

### Added

- Json-rpc based API, see the docs for more info.
- Message forwarding to unix sockets if configured.
- Config file support to handle messages, if this is enabled.

### Changed

- Routing has been reworked. We no longer advertise selected subnets (which aren't
  our own). Now if a subnet is needed, we perform a route request for that subnet,
  memorizing state and responses. The current imlementation expires routes every 5
  minutes but does not yet refresh active routes before they expire.
- Before we process a seqno request for a subnet, check the seqno cache to see if
  we recently forwarded an entry for it.
- Discard Update TLV's if there are too many in the queue already. This binds memory
  usage but can cause nodes with a lot of load to not pick up routes immediately.

## [0.5.7] - 2024-11-31

### Fixed

- Properly set the interface ipv6 MTU on windows.

## [0.5.6] - 2024-10-03

### Fixed

- Fix a panic in the route cleanup task when a peer dies who is the last stored
  announcer of a subnet.

## [0.5.5] - 2024-09-27

### Added

- Mycelium-ui, a standalone GUI which exposes (part of) the mycelium API. This
  does **not** have a bundled mycelium node, so that needs to be run separately.

### Changed

- Default TUN name on Linux and Windows is now `mycelium`. On MacOS it is now `utun0`.
- TUN interface name validation on MacOS. If the user supplies an invalid or already
  taken interface name, an available interface name will be automatically assigned.

### Fixed

- Release flow to create the Windows installer now properly extracts wintun
- Guard against a race condition when a route is deleted which could rarely
  trigger a panic and subsequent memory leak.

## [0.5.4] - 2024-08-20

### Added

- Quic protocol can now be disabled using the `--disable-quic` flag
- Mycelium can now be started with a configuration file using `--config-file`.
  If no configuration file is supplied, Mycelium will look in a default location
  based on the OS. For more information see [README](/README.md#configuration)
- Windows installer for Mycelium. The `.msi` file can be downloaded from the release
  assets.
- Added flag to specify how many update workers should be started, which governs
  the amount of parallelism used for processing updates.
- Send a seqno request if we receive an unfeasible update for a subnet with no
  routes, or if there is no selected route for the subnet.
- New public peers in US, India, and Singapore.

### Changed

- Increased the starting metric of a peer from 50 to 1000.
- Reworked the internals of the routing table, which should reduce memory consumption.
  Additionally, it is now possible to apply updates in parallel
- Periodically reduce the allocated size of the seqno cache to avoid wasting some
  memory which is not currently used by the cache but still allocated.
- Demote seqno cache warnings about duplicate seqno requests go debug lvl, as it
  is valid to send duplicate requests if sufficient time passed.
- Skip route selection after an unfeasible update to a fallback route, as the (now
  unfeasible) route won't be selected anyway.
- No longer refresh route timer after an unfeasbile update. This allows routes
  which have become unfeasible to gracefully be removed from the routing table
  over time.
- Expired routes which aren't selected are now immediately removed from the routing
  table.
- Changed how updates are sent to be more performant.
- A triggered update is no longer sent just because a route sequence number got
  increased. We do still send the update to peer in the seqno request cache.
- Reduced log level when a route changes next-hop to debug from info.

### Fixed

- When running `mycelium` with a command, a keyfile was loaded (or created, if not
  yet present). This was not necessary in that context.
- Limit the amount of time allowed for inbound quic connections to be set up, and
  process multiple of them in parallel. This fixes a DOS vector against the quic
  listener.
- We now update the source table even if we don't send an update because we are
  sure the receiver won't select us as a next-hop anyway.

## [0.5.3] - 2024-06-07

### Added

- On Linux and macOS, a more descriptive error is printed when setting up the tun
  device fails because a device with the same name already exists.
- Seqno request cache, to avoid spamming peers with duplicate seqno requests and
  to make sure seqno's are forwarded to different peers.
- Added myceliumd-private binary, which contains private network functionality.
- Added API endpoint to retrieve the public key associated with an IP.
- The CLI can now be used to list, remove or add peers (see `mycelium peers --help`)
- The CLI can now be used to list selected and fallback routes (see
  `mycelium routes --help`)

### Changed

- We now send seqno requests to all peers who advertised a subnet if the selected
  route to it is lost as a result of the next-hop dying, or and update coming in
  which causes no routes to be feasible anymore.
- Switched from the log to the tracing ecosystem.
- Only do the periodic route announcement every 5 minutes instead of every minute.
- Mycelium binary is no longer part of the workspace, and no longer contains private
  network functionality.
- If a packet received from a peer can't be forwarded to the router, terminate the
  connection to the peer.

### Fixed

- Manually implement Hash for Subnet, previously we could potentially have multiple
  distinct entries in the source table for the same source key.

## [0.5.2] - 2024-05-03

### Added

- New CI workflow to build and test the mycelium library separately from the full
  provided binary build.

### Changed

- Disabled the protobuf feature on prometheus, this removes protobuf related code
  and significantly reduces the release binary size.
- Changed log level when sending a protocol message to a peer which is no longer
  alive from error to trace in most instances.
- Improved performance of sending protocol messages to peers by queueing up multiple
  packets at once (if multiple are ready).
- Before trying to send an update we now check if it makes sense to do so.
- If a peer died, fallback routes using it are no longer retained with an infinite
  metric but removed immediately.
- No longer run route selection for subnets if a peer died and the route is not
  selected.
- If routes are removed, shrink the capacity of the route list in the route table
  if it is larger than required.
- Check if the originator of a TLV is still available before processing said TLV.
- The router now uses a dedicated task per TLV type to handle received TLV's from
  peers.
- Statically linking openssl is now a feature flag when building yourself.

### Fixed

- If a peer died, unselect the selected routes which have it as next-hop if there
  is no other feasible route.
- Properly unselect a route if a retraction update comes in and there is no other
  feasible route.
- If the router bumps it's seqno it now properly announces the local route to it's
  peers instead of the selected routes
- Seqno bump requests for advertised local routes now properly bump the router
  seqno.

## [0.5.1] - 2024-04-19

### Added

- The repo is now a workspace, and pure library code is separated out. This is mainly
  done to make it easier to develop implementations on different platforms.
- Link local discovery will now send discovery beacons on every interface the process
  listens on for remote beacons.
- Experimental private network support. See [the private network docs](/docs/private_network.md)
  for more info.
- You can now optionally expose Prometheus compatible metrics about the system by
  setting the --metrics-api-address flag.
- On Linux, you can now set an optional firewall mark by setting the --firewall-mark
  flag.
- Added a nix flake to the repo.

### Changed

- We no longer create an outbound connection to a link local discovered IP if that
  IP is already known (usually as inbound address) with potentially a different
  port.

## [0.5.0] - 2024-04-04

### Changed

- Connection identifier is now included in the error log if we can't forward a
  seqno request.
- Garbage collection time for source entries has been increased from 5 to 30 minutes
  for now.
- The router implementation has been changed to use regular locks instead of an
  always readable concurrency primitive for all but the actual routing table. This
  should reduce the memory consumption a bit.
- Public key and shared secret for a destination are now saved on the router, instead
  of maintaining a separate mapping for them. This slightly reduces memory consumption
  of the router, and ensures stale data is properly cleaned up when all routes to
  a subnet are removed.
- Hello packets now set the interval in which the next Hello will be sent properly
  in centiseconds.
- IHU packets now set the interval properly in centiseconds.
- IHU packets now set an RX cost. For now this is the link cost, in the future
  this will be set properly.
- Route expiration time is now calculated from the interval received in updates.
- Ip address derivation from public keys now uses the blake3 hash algorithm.

### Fixed

- Don't try to forward seqno requests to a peer if we know its connection is dead.

## [0.4.5] - 2024-03-26

### Changed

- Size of data packets is limited to 65535 bytes.
- Update interval is now expressed as centiseconds, in accordance with the babel
  RFC.
- Update filters now allow retractions for a route from any router-id.

### Fixed

- The feasibility distance of an existing source key is no longer incorrectly updated
  when the metric increases.
- Source key garbage collection timers are properly reset on update even if the
  source key itself is not updated.
- Nodes now properly reply to route requests for a static route.
- A retraction is now sent as reply to a route request if the route is not known.

## [0.4.4] - 2024-03-22

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

## [0.1.0] - 2023-11-15

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
[0.4.4]: https://github.com/threefoldtech/mycelium/compare/v0.4.3...v0.4.4
[0.4.5]: https://github.com/threefoldtech/mycelium/compare/v0.4.4...v0.4.5
[0.5.0]: https://github.com/threefoldtech/mycelium/compare/v0.4.5...v0.5.0
[0.5.1]: https://github.com/threefoldtech/mycelium/compare/v0.5.0...v0.5.1
[0.5.2]: https://github.com/threefoldtech/mycelium/compare/v0.5.1...v0.5.2
[0.5.3]: https://github.com/threefoldtech/mycelium/compare/v0.5.2...v0.5.3
[0.5.4]: https://github.com/threefoldtech/mycelium/compare/v0.5.3...v0.5.4
[0.5.5]: https://github.com/threefoldtech/mycelium/compare/v0.5.4...v0.5.5
[0.5.6]: https://github.com/threefoldtech/mycelium/compare/v0.5.5...v0.5.6
[0.5.7]: https://github.com/threefoldtech/mycelium/compare/v0.5.6...v0.5.7
[0.6.0]: https://github.com/threefoldtech/mycelium/compare/v0.5.7...v0.6.0
[0.6.1]: https://github.com/threefoldtech/mycelium/compare/v0.6.0...v0.6.1
[0.6.2]: https://github.com/threefoldtech/mycelium/compare/v0.6.1...v0.6.2
[0.7.0]: https://github.com/threefoldtech/mycelium/compare/v0.6.2...v0.7.0
[unreleased]: https://github.com/threefoldtech/mycelium/compare/v0.7.0...HEAD
