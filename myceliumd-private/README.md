# Myceliumd-private

This is the main binary to use when connecting to [a private network](../docs/private_network.md).
You can either get a release from [the GitHub release page](https://github.com/threefoldtech/myceliumd/releases), 
or build the code yourself here. While we intend to keep the master branch as stable,
i.e. a build from latest master should succeed, this is not a hard guarantee. Additionally,
the master branch might not be compatible with the latest release, in case of a
breaking change.

Building, with the rust toolchain installed, can be done by running `cargo build`
in this directory. Since we rely on `openssl` for the private network functionality,
that will also need to be installed. The produced binary will be dynamically linked
with this system installation. Alternatively, you can use the `vendored-openssl`
feature to compile `openssl` from source and statically link it. You won't need
to have `openssl` installed, though building will of course take longer. To stay
in line with the regular `mycelium` binary, the produced binary here is renamed
to `mycelium-private`. Optionally, the `--release` flag can be used when building.
This is recommended if you are not just developing.

While this binary can currently be used to connect to the public network, it is
not guaranteed that this will always be the case. If you intend to connect to the
public network, consider using [`the mycelium binary`](../myceliumd/README.md)
instead.

For more information about the project, please refer to [the README file in the
repository root](../README.md).
