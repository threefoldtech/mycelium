# Mycelium

This POC aims to create an IPv6 overlay network completely writing in Rust. The overlay network uses some of the core principles
of the Babel routing protocol (https://www.irif.fr/~jch/software/babel/). Each node that joins the overlay network will receive
an overlay network IP in the 200::/7 range. 

## Running

> Currently, only Linux is supported.

First, get an useable binary, either by downloading [an artifact from a release](https://github.com/threefoldtech/mycelium/releases),
or by [checking out and building the code yourself](#Developing).

Once you have an useable binary, simply start it. If you want to connect to other nodes, you can specify their listening address as
part of the command

```sh
mycelium -peers 203.0.113.5:9651
```

By default, the node will listen on port `9651`, though this can be overwritten with the `-p` flag.

The node uses a `x25519` keypair from which its identity is derived. The private key of this keypair is saved in a local file (32 bytes in binary format). You can
specify the path to this file with the `-k` flag. By default, the file is saved in the current working directory as `priv_key.bin`.

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
