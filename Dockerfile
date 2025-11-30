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

# FROM debian:latest AS base
# COPY --from=daemonBuild /bin/mycelium /bin/mycelium

# entrypoint
ENTRYPOINT /bin/mycelium

