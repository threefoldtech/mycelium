# Myceliumd

This is the main binary to use for joining a/the public [`mycelium`] network. You can
either get the latest release from [the GitHub release page](https://github.com/threefoldtech/myceliumd/releases/latest),
or build the code yourself here. While we intend to keep the master branch as stable,
i.e. a build from latest master should succeed, this is not a hard guarantee. Additionally,
the master branch might not be compatible with the latest release, in case of a
breaking change.

Building, with the rust toolchain installed, can be done by running `cargo build`
in this directory. For compatibility reasons, the binary is renamed to `mycelium`.
Optionally, the `--release` flag can be used when building. This is recommended
if you are not just developing.

For more information about the project, please refer to [the README file in the
repository root](../README.md).
