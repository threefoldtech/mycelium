FROM debian:latest AS build

# TODO: Add nonnteractive flag for apt

# Upgrade system
RUN apt update
RUN apt upgrade -y

# Install build dependencies
RUN apt install rustc -y

# Copy across data
RUN mkdir /src
COPY . /src
WORKDIR /src

# target: myceliumd - the routing daemon
FROM build AS daemonBuild
WORKDIR myceliumd/
RUN cargo build
RUN mv target/debug/mycelium /bin/mycelium

# TODO: Add copying across of other tools like cli management etc.
# and probably build them seperately

# Clean base image to run fro
FROM debian:latest AS base
COPY --from=daemonBuild /bin/mycelium /bin/mycelium

ENV MYCELIUM_PEERS_STRING=""

ENV MYCELIUM_QUIC_PORT=9651
ENV MYCELIUM_TCP_PORT=9651
ENV MYCELIUM_PD_PORT=9650
ENV MYCELIUM_TUN_IFNAME=mycelium0

# Command to run (FIXME: Remove `--debug`)
CMD [ \
			"/bin/mycelium", "--debug", \
			"--key-file", "/data/private.key", \
			"--peers", $MYCELIUM_PEERS_STRING, \
\
			"--quic-listen-port", $MYCELIUM_QUIC_PORT, \
      "--tcp-listen-port", $MYCELIUM_TCP_PORT, \
      "--peer-discovery-port", $MYCELIUM_PD_PORT, \
\
      "--tun-name", $MYCELIUM_TUN_IFNAME \
		]
