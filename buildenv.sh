#!/usr/bin/env bash
# Project identity
export PROJECT_NAME="mycelium"

# Binaries to build (produced by myceliumd and myceliumd-private workspaces)
export BINARIES="mycelium mycelium-private"

# Cargo feature flags — no extra features for default build
# Note: binaries live in separate sub-workspaces (myceliumd/, myceliumd-private/)
# and are built with --manifest-path in the Makefile rather than build_binaries()
export ALL_FEATURES="default"

export VERSION="0.7.5"
export PATCHLEVEL="patch"
