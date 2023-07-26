# Mycelium

This POC aims to create an IPv6 overlay network completely writing in Rust. The overlay network uses some of the core principles of the Babel routing protocol (https://www.irif.fr/~jch/software/babel/). Each node that joins the overlay network will receive an overlay network IP in the 200::/7 range. 

## Installation

```sh
git clone git@github.com:LeeSmet/mycelium.git
# in case of an exisiting route to 200::/7, it should be deleted in order to run the application
ip r del 200::/7
```

## Usage example
In this example setup we create a small overlay network on 5 different virtual machines that looks like this:


```
+----------+        +----------+        +--------+
| node 1   |--------| node 2   |--------| node 3 |
+----------+        +----------+        +--------+
     |                                   |
     |   +--------+        +-------+     |
     +---| node 4 |--------| node 5|-----+
         +--------+        +-------+
```
To create this setup, run the following commands:

```sh
# node 1
cargo run
# node 2
cargo run -- -p <IP address node 1>
# node 3
cargo run -- -p <IP address node 2>
# node 4
cargo run -- -p <IP address node 1>
# node 5
cargo run -- -p <IP address node 4> -p <IP address node 3>
```


