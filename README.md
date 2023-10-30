# Mycelium

This POC aims to create an IPv6 overlay network completely writing in Rust. The overlay network uses some of the core principles
of the Babel routing protocol (<https://www.irif.fr/~jch/software/babel/>). Each node that joins the overlay network will receive
an overlay network IP in the 200::/7 range.

## Running

> Currently, only Linux is supported.

First, get an useable binary, either by downloading [an artifact from a release](https://github.com/threefoldtech/mycelium/releases),
or by [checking out and building the code yourself](#developing).

Once you have an useable binary, simply start it. If you want to connect to other nodes, you can specify their listening address as
part of the command

```sh
mycelium --peers 203.0.113.5:9651
```

By default, the node will listen on port `9651`, though this can be overwritten with the `-p` flag.

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

## API

The node starts an HTTP API, which by default listens on `localhost:8989`. A different listening address
can be specified on the CLI when starting the system through the `--api-server-addr` flag. The API
allows access to [send and receive messages](#message-system), and will later be expanded to allow
admin functionality on the system. Note that message are sent using the identity of the node, and a
future admin API can be used to change the system behavior. As such, care should be taken that this
API is not accessible to unauthorized users.

## Message system

A message system is provided which allows users to send a message, which is essentially just "some data"
to a remote. Since the system is end-to-end encrypted, a receiver of a message is sure of the authenticity
and confidentiality of the content. The system does not interpret the data in any way and handles it
as an opaque block of bytes. Messages are sent with a deadline. This means the system continuously
tries to send (part of) the message, until it either succeeds, or the deadline expires. This happens
similar to the way TCP handles data. Messages are transmitted in chunks, which are embedded in the
same data stream used by L3 packets. As such, intermediate nodes can't distinguish between regular L3
and message data.

The primary way to interact with the message system is through [the API](#API). The message API is
documented in [an OpenAPI spec in the docs folder](docs/api.yaml).


## Inspecting node keys

Using the `inspect` subcommand, you can view the address associated with a public key. If no public key is provided, the node will show
its own public key. In either case, the derived address is also printed. You can specify the path to the private key with the `-k` flag.
If the file does not exist, a new private key will be generated. The optional `--json` flag can be used to print the information in json
format.

```sh
mycelium inspect a47c1d6f2a15b2c670d3a88fbe0aeb301ced12f7bcb4c8e3aa877b20f8559c02
Public key: a47c1d6f2a15b2c670d3a88fbe0aeb301ced12f7bcb4c8e3aa877b20f8559c02
Address: 27f:b2c5:a944:4dad:9cb1:da4:8bf7:7e65
```

```sh
mycelium inspect --json
{
  "publicKey": "955bf6bea5e1150fd8e270c12e5b2fc08f08f7c5f3799d10550096cc137d671b",
  "address": "34f:b680:ba6e:7ced:355f:346f:d97b:eecb"
}
```

## Developing

This project is built in Rust, and you must have a rust compiler to build the code yourself. Please refer to [the official rust documentation](https://www.rust-lang.org/)
for information on how to install `rustc` and `cargo`. Aside from the rust toolchain, no other dependencies should be required to
build the project.

First make sure you have cloned the repo

```sh
git clone https://github.com/threefoldtech/mycelium.git
```

Then go into the cloned directory and build it

```sh
cd mycelium
cargo build
```

In case a release build is required, the `--release` flag can be added to the cargo command (`cargo build --release`).
