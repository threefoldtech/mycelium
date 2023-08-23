#!/usr/bin/env bash
#
# A simple shell script to setup a local network using network namespaces. This relies on the presence of the `ip` tool, part of `iproute2` packge on linux.
#
# Note: this script requires root privilege

set -ex

ip l add p0 type veth peer p1
ip l add p2 type veth peer p3
ip l add p4 type veth peer p5

ip l add mycelium-br type bridge

ip l set p1 master mycelium-br
ip l set p3 master mycelium-br
ip l set p5 master mycelium-br

ip netns add net1
ip netns add net2

ip -n net1 l set lo up
ip -n net2 l set lo up

ip l set p2 netns net1
ip l set p4 netns net2

ip a add 10.0.2.1/24 dev p0 
ip -n net1 a add 10.0.2.2/24 dev p2
ip -n net2 a add 10.0.2.3/24 dev p4

ip l set p0 up
ip l set p1 up
ip -n net1 l set p2 up
ip l set p3 up
ip -n net2 l set p4 up
ip l set p5 up
ip l set mycelium-br up
