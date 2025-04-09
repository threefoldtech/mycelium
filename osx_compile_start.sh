#!/bin/bash

# Exit on error
set -e

# Change to the project directory
cd "$(dirname "$0")"

# Build the project
echo "Building myceliumd..."
cd myceliumd
cargo build --features message

# Start myceliumd with sudo
echo "Starting myceliumd..."
# Use full path for sudo
FULL_PATH="$(pwd)/target/debug/mycelium"
sudo "$FULL_PATH" --unix-socket-path /tmp/mycelium.sock

# Note: You may need to provide your password for sudo