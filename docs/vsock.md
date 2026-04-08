# Vsock transport

Mycelium supports [VM sockets (vsock)](https://man7.org/linux/man-pages/man7/vsock.7.html) as a
transport, allowing a mycelium node running inside a virtual machine to peer directly with a node
running on the hypervisor host (or vice versa), without requiring a network interface.

Vsock is only available on Linux.

## How it works

Vsock addresses a device by its **Context ID (CID)** and a **port number**, rather than an IP
address. Key CID values:

| CID | Meaning |
|-----|---------|
| `2` | The hypervisor host |
| `3` and up | Guest VMs (assigned by the hypervisor) |
| `4294967295` | Any CID (used when listening) |

A node that wants to accept inbound vsock connections binds to `VMADDR_CID_ANY` on a given port.
The connecting side targets the peer's CID explicitly.

## Starting a node with a vsock listener

Pass `--vsock-listen-port` when starting `mycelium` to accept inbound vsock connections:

```sh
mycelium --vsock-listen-port 9651
```

The listener binds on all CIDs (`VMADDR_CID_ANY`), so both host-to-guest and guest-to-host
connections are accepted on the same port.

## Connecting to a peer over vsock

Vsock peers are specified with the `vsock://` scheme in the endpoint format:

```
vsock://<CID>:<port>
```

### Example: guest VM connecting to the host

On the **host**, start mycelium with a vsock listener:

```sh
mycelium --vsock-listen-port 9651
```

On the **guest VM**, connect to the host (CID `2`) as a static peer:

```sh
mycelium --peers vsock://2:9651
```

### Example: host connecting to a guest VM

Find the guest's CID by consulting your hypervisor's documentation.

On the **guest**, start mycelium with a vsock listener:

```sh
mycelium --vsock-listen-port 9651
```

On the **host**, connect to the guest using its CID (e.g. `42`):

```sh
mycelium --peers vsock://42:9651
```

## Configuration file

Vsock settings can also be placed in the mycelium configuration file:

```toml
# Listen for inbound vsock connections on this port
vsock_listen_port = 9651

# Connect to the hypervisor host as a static peer
peers = ["vsock://2:9651"]
```

## Combining vsock with other transports

Vsock and IP-based transports can be used simultaneously. A node can listen on
both TCP and vsock at the same time, and peer over both:

```sh
mycelium --tcp-listen-port 9651 --vsock-listen-port 9651 --peers vsock://2:9651
```

## Link cost

The vsock transport is assigned a lower link cost than any IP-based transport,
reflecting that it is a local virtual channel with negligible latency. Mycelium
will prefer a vsock path over an IP path when both are available to the same
destination.
