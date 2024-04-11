# Private network

> Private network functionality is currently in an experimental stage

While traffic is end-to-end encrypted in mycelium, any node in the network learns
every available connected subnet (and can derive the associated default address
in that subnet). As a result, running a mycelium node adds what is effectively
a public interface to your computer, so everyone can send traffic to it. On top
of this, the routing table consumes memory in relation to the amount of nodes in
the network. To remedy this, people can opt to run a "private network". By configuring
a pre shared key (and network name), only nodes which know the key associated to
the name can connect to your network.

## Implementation

Private networks are implemented entirely in the connection layer (no specific
protocol logic is implemented to support this). This relies on the pre shared key
functionality of TLS 1.3. As such, you need both a `network name` (an `identity`),
and the `PSK` itself. Next to the limitations in the protocol, we currently further
limit the network name and PSK as follows:

- Network name must be a UTF-8 encoded string of 2 to 64 bytes.
- PSK must be exactly 32 bytes.

## Enable private network

If you are running the standard `mycelium` binary, private network functionality
can be enabled by setting both the `network-name` and `network-key` flags on the
command line. All nodes who which to join the network must use the same values
for both flags.

> ⚠️ Network name is public, do not put any confidential data here.
