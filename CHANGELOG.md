# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- CHANGELOG.md file to keep track of notable additions, changes, fixes, deprecations, and removals.
- A peer can now detect if it is no longer useable in most cases, allowing it to notify the router if it died. This
  allows near instant retraction of routes if a connection dies, decreasing the amount of time needed to find a
  suitable alternative route.

### Changed

- When a peer is found to be dead, routes which use it as next-hop now have their metric set to infinity.
  If the route is selected, route selection for the subnet is run again and if needed a triggered update is sent.
  This will allow downstream peers to receive a timely update informing them of a potentially retracted route,
  instead of having to wait for route expiration.

## [0.1.0] - 2023-11-15

### Added

- Initial working setup with end-to-end encryption of traffic.
- Ability to specify peers to connect to
- Message subsystem, including API to use it.
- CLI options to send and receive messages with the API.

[unreleased]: https://github.com/threefoldtech/mycelium/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/threefoldtech/mycelium/releases/tag/v0.1.0
