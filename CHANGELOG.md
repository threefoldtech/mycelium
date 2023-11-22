# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Add info log when next hop of a peer changes.

### Changed

- When printing the connected peers, print the underlay IP instead of the overlay IP.
- The link cost of a peer is now the smoothed average. This makes sure a single short latency spike doesn't disrupt routing.
- On Linux, set the TUN ip as /7 and avoid setting a /64 route. This brings it in line with OSX.
- When selecting the best route for a subnet, consider the currently
  installed route and only switch if it is significantly better, or
  directly connected
- Increase the static link cost component of a peer. This will increase
  the value of a hop in the metric of a route, in turn increasing the
  impact of multiple hops on route selection. The route selection will
  thus be more inclined to find a path with fewer hops toward a
  destination. Ultimately, if multiple paths to a destination exist with
  roughly the same latency, they one with fewer hops should be
  preferred, since this avoids putting unnecessary pressure on multiple
  nodes in the network.

### Fixed

- Ignore retraction updates if the route table has an existing but retracted route already. This fixes
  an issue where retracted routes would not be flushed from the routing table.

## [0.1.2] - 2023-11-15

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

[unreleased]: https://github.com/threefoldtech/mycelium/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/threefoldtech/mycelium/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/threefoldtech/mycelium/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/threefoldtech/mycelium/releases/tag/v0.1.0
