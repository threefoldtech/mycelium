# Mycelium

Mycelium is an IPv6 overlay network written in Rust. Each node that joins the overlay
network will receive an overlay network IP in the 400::/7 range.

## Features

- Mycelium, is locality aware, it will look for the shortest path between nodes
- All traffic between the nodes is end-2-end encrypted
- Traffic can be routed over nodes of friends, location aware
- If a physical link goes down Mycelium will automatically reroute your traffic
- The IP address is IPV6 and linked to private key
- A simple reliable messagebus is implemented on top of Mycelium
- Mycelium has multiple ways how to communicate quic, tcp, ... and we are working on holepunching for Quick which means P2P traffic without middlemen for NATted networks e.g. most homes
- Scalability is very important for us, we tried many overlay networks before and got stuck on all of them, we are trying to design a network which scales to a planetary level
- You can run mycelium without TUN and only use it as reliable message bus.

> We are looking for lots of testers to push the system

> [see here for docs](https://github.com/threefoldtech/mycelium/tree/master/docs)

## Running

> Currently, Linux, macOS and Windows are supported.

### Linux and macOS

Get an useable binary, either by downloading [an artifact from a release](https://github.com/threefoldtech/mycelium/releases),
or by [checking out and building the code yourself](#developing).

> On Mac: make sure to use `xattr` on the archive before extracting it e.g `xattr -cr  mycelium-aarch64-apple-darwin.tar.gz`

### Windows

Download the [mycelium_installer.msi](https://github.com/threefoldtech/mycelium/releases/latest/download/mycelium_installer.msi) and run the installer.

### Run Mycelium

Once you have an useable binary, simply start it. If you want to connect to other
nodes, you can specify their listening address as part of the command (combined
with the protocol they are listening on, usually TCP). Check the next section if
you want to connect to hosted public nodes.

```sh
mycelium --peers tcp://188.40.132.242:9651 quic://185.69.166.8:9651

#other example with other tun interface if utun3 (the default) would already be used
#also here we use sudo e.g. on OSX
sudo mycelium --peers tcp://188.40.132.242:9651 quic://185.69.166.8:9651 --tun-name utun9

```

By default, the node will listen on port `9651`, though this can be overwritten
with the `-p` flag.

To check your own info

```bash
mycelium inspect --json
{
  "publicKey": "abd16194646defe7ad2318a0f0a69eb2e3fe939c3b0b51cf0bb88bb8028ecd1d",
  "address": "5c4:c176:bf44:b2ab:5e7e:f6a:b7e2:11ca"
}
# test that network works, ping to anyone in the network
ping6 54b:83ab:6cb5:7b38:44ae:cd14:53f3:a907
```

The node uses a `x25519` key pair from which its identity is derived. The private key of this key pair
is saved in a local file (32 bytes in binary format). You can specify the path to this file with the
`-k` flag. By default, the file is saved in the current working directory as `priv_key.bin`.

### Running without TUN interface

It is possible to run the system without creating a TUN interface, by starting with the `--no-tun` flag.
Obviously, this means that your node won't be able to send or receive L3 traffic. There is no interface
to send packets on, and consequently no interface to send received packets out of. From the point of
other nodes, your node will simply drop all incoming L3 traffic destined for it. The node **will still
route traffic** as normal. It takes part in routing, exchanges route info, and forwards packets not
intended for itself.

The node also still allows access to the [message subsystem](#message-system).

## Configuration

Mycelium can be started with an **optional** configuration file using the `--config-file`
option, which offers the same capabilities as the command line arguments.

If no configuration file is specified with `--config-file`, Mycelium will search for one
in a default location based on the operating system:

- Linux: $HOME/.config/mycelium.toml
- Windows: %APPDATA%/ThreeFold Tech/Mycelium/mycelium.toml
- Mac OS: $HOME/Library/Application Support/ThreeFold Tech/Mycelium/mycelium.toml

Command line arguments will override any settings found in the configuration file.

## Hosted public nodes v0.6.x

A couple of public nodes are provided, which can be freely connected to. This allows
anyone to join the global network. These are hosted in multiple geographic regions,
on both IPv4 and IPv6, and supporting both the Tcp and Quic protocols. The nodes
are the following:

| Node ID | Region | IPv4 | IPv6 | Tcp port | Quic port | Mycelium IP |
| ------- | ------- | -------------- | --------------------------------- | -------- | --------- | -------------------------------------- |
| 01 | DE | 188.40.132.242 | 2a01:4f8:221:1e0b::2 | 9651 | 9651 | 54b:83ab:6cb5:7b38:44ae:cd14:53f3:a907 |
| 02 | DE | 136.243.47.186 | 2a01:4f8:212:fa6::2 | 9651 | 9651 | 40a:152c:b85b:9646:5b71:d03a:eb27:2462 |
| 03 | BE | 185.69.166.7 | 2a02:1802:5e:0:ec4:7aff:fe51:e80d | 9651 | 9651 | 597:a4ef:806:b09:6650:cbbf:1b68:cc94 |
| 04 | BE | 185.69.166.8 | 2a02:1802:5e:0:ec4:7aff:fe51:e36b | 9651 | 9651 | 549:8bce:fa45:e001:cbf8:f2e2:2da6:a67c |
| 05 | FI | 65.21.231.58 | 2a01:4f9:6a:1dc5::2 | 9651 | 9651 | 410:2778:53bf:6f41:af28:1b60:d7c0:707a |
| 06 | FI | 65.109.18.113 | 2a01:4f9:5a:1042::2 | 9651 | 9651 | 488:74ac:8a31:277b:9683:c8e:e14f:79a7 |
| 07 | US-EAST | 209.159.146.190 | 2604:a00:50:17b:9e6b:ff:fe1f:e054 | 9651 | 9651 | 4ab:a385:5a4e:ef8f:92e0:1605:7cb6:24b2 |
| 08 | US-WEST | 5.78.122.16 | 2a01:4ff:1f0:8859::1 | 9651 | 9651 | 4de:b695:3859:8234:d04c:5de6:8097:c27c |
| 09 | SG | 5.223.43.251 | 2a01:4ff:2f0:3621::1 | 9651 | 9651 | 5eb:c711:f9ab:eb24:ff26:e392:a115:1c0e |
| 10 | IND | 142.93.217.194 | 2400:6180:100:d0::841:2001 | 9651 | 9651 | 445:465:fe81:1e2b:5420:a029:6b0:9f61 |

These nodes are all interconnected, so 2 peers who each connect to a different node
(or set of disjoint nodes) will still be able to reach each other. For optimal performance,
it is recommended to connect to all of the above at once however. An example connection
string could be:

`--peers tcp://188.40.132.242:9651 "quic://[2a01:4f8:212:fa6::2]:9651" tcp://185.69.166.7:9651 "quic://[2a02:1802:5e:0:ec4:7aff:fe51:e36b]:9651" tcp://65.21.231.58:9651 "quic://[2a01:4f9:5a:1042::2]:9651" "tcp://[2604:a00:50:17b:9e6b:ff:fe1f:e054]:9651" quic://5.78.122.16:9651 "tcp://[2a01:4ff:2f0:3621::1]:9651" quic://142.93.217.194:9651`

It is up to the user to decide which peers he wants to use, over which protocol.
Note that quotation may or may not be required, depending on which shell is being
used. IPv6 addresses should of course only be used if your ISP provides you with
IPv6 connectivity.

### Private network

Mycelium supports running a private network, in which you must know the network name
and a PSK (pre shared key) to connect to nodes in the network. For more info, check
out [the relevant docs](/docs/private_network.md).

## API

The node starts an HTTP API, which by default listens on `localhost:8989`. A different
listening address can be specified on the CLI when starting the system through the
`--api-addr` flag. The API allows access to [send and receive messages](#message-system),
and will later be expanded to allow admin functionality on the system. Note that
message are sent using the identity of the node, and a future admin API can be
used to change the system behavior. As such, care should be taken that this API
is not accessible to unauthorized users.

## Message system

A message system is provided which allows users to send a message, which is essentially just "some data"
to a remote. Since the system is end-to-end encrypted, a receiver of a message is sure of the authenticity
and confidentiality of the content. The system does not interpret the data in any way and handles it
as an opaque block of bytes. Messages are sent with a deadline. This means the system continuously
tries to send (part of) the message, until it either succeeds, or the deadline expires. This happens
similar to the way TCP handles data. Messages are transmitted in chunks, which are embedded in the
same data stream used by L3 packets. As such, intermediate nodes can't distinguish between regular L3
and message data.
The primary way to interact with the message system is through [the API](#api). The message API is
documented in [an OpenAPI spec in the docs folder](docs/api.yaml). For some more info about how to
use the message system, see [the message docs](/docs/message.md).

Messages can be categorized by topics, which can be configured with whitelisted subnets and socket forwarding paths.
For detailed information on how to configure topics, see the [Topic Configuration Guide](/docs/topic_configuration.md).
use the message system, see [the message docs](/docs/message.md).

## Inspecting node keys

Using the `inspect` subcommand, you can view the address associated with a public key. If no public key is provided, the node will show
its own public key. In either case, the derived address is also printed. You can specify the path to the private key with the `-k` flag.
If the file does not exist, a new private key will be generated. The optional `--json` flag can be used to print the information in json
format.

```sh
mycelium inspect a47c1d6f2a15b2c670d3a88fbe0aeb301ced12f7bcb4c8e3aa877b20f8559c02
Public key: a47c1d6f2a15b2c670d3a88fbe0aeb301ced12f7bcb4c8e3aa877b20f8559c02
Address: 47f:b2c5:a944:4dad:9cb1:da4:8bf7:7e65
```

```sh
mycelium inspect --json
{
  "publicKey": "955bf6bea5e1150fd8e270c12e5b2fc08f08f7c5f3799d10550096cc137d671b",
  "address": "54f:b680:ba6e:7ced:355f:346f:d97b:eecb"
}
```

## Developing

This project is built in Rust, and you must have a rust compiler to build the code
yourself. Please refer to [the official rust documentation](https://www.rust-lang.org/)
for information on how to install `rustc` and `cargo`. This project is a workspace,
however the binaries (`myceliumd` and `myceliumd-private`) are explicitly _not_
part of this workspace. The reason for this is the way the cargo resolver unifies
features. Making both binaries part of the workspace would make the library build
for the regular binary include the code for the private network, and since that
is internal code it won't be removed at link time.

First make sure you have cloned the repo

```sh
git clone https://github.com/threefoldtech/mycelium.git
cd mycelium
```

If you only want to build the library, you can do so from the root of the repo

```sh
cargo build
```

If you instead want to build a binary, that must be done from the appropriate subdirectory

```sh
cd myceliumd
cargo build
```

Refer to the README files in those directories for more info.

In case a release build is required, the `--release` flag can be added to the cargo
command (`cargo build --release`).

## Cross compilation

For cross compilation, it is advised to use the [`cross`](https://github.com/cross-rs/cross)
project. Alternatively, the standard way of cross compiling in rust can be used
(by specifying the `--target` flag in the `cargo build` command). This might require
setting some environment variables or local cargo config. On top of this, you should
also provide the `vendored-openssl` feature flag to build and statically link a copy
of openssl.

## Remarks

- The overlay network uses some of the core principles of the Babel routing protocol (<https://www.irif.fr/~jch/software/babel/>).
