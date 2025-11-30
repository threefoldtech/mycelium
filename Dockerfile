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

# Bring in all environment variables
ENV PEERS_STRING=""

ENV QUIC_PORT=9651
ENV TCP_PORT=9651
ENV PD_PORT=9650

ENV TUN_IFNAME=mycelium0

ENV LOG_OPTION=debug

# Entrypoint
ENTRYPOINT /bin/mycelium

# Arguments to entry point
CMD ["--$LOG_OPTION", "--key-file", "/data/private.key", "--peers", "$PEERS_STRING", "--quic-listen-port", "$QUIC_PORT", "--tcp-listen-port", "$TCP_PORT", "--peer-discovery-port", "$PD_PORT", "--tun-name", "$TUN_IFNAME"]

# TODO: Add health-check command
# HEALTHCHECK /bin/mycelium