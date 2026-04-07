# ============================================================================
# Project Configuration
# ============================================================================

# Ensure bash is used (needed for `source` command)
SHELL := /bin/bash

BUILD_LIB := scripts/build_lib.sh

VERSION     := $(shell bash -c 'source $(BUILD_LIB) 2>/dev/null && echo $$VERSION')
INSTALL_DIR := $(shell bash -c 'source $(BUILD_LIB) 2>/dev/null && echo $$INSTALL_DIR')

# Ensure cargo is in PATH for all cargo-using targets
CARGO_ENV := source $(BUILD_LIB) 2>/dev/null && cargo_env &&

DAEMON_RELEASE_DIR  := myceliumd/target/release
DAEMON_DEBUG_DIR    := myceliumd/target/debug
PRIVATE_RELEASE_DIR := myceliumd-private/target/release
PRIVATE_DEBUG_DIR   := myceliumd-private/target/debug

BINARY_DAEMON  := mycelium
BINARY_PRIVATE := mycelium-private

# ============================================================================
# Phony Targets
# ============================================================================

.PHONY: help build build-daemon build-private build-all \
        install install-daemon install-private install-all \
        installdev installdev-daemon installdev-private \
        run stop restart dev check fmt fmt-check lint \
        test clean all version status \
        update-deps

# ============================================================================
# Help & Information
# ============================================================================

help: ## Display all available targets
	@echo "mycelium v$(VERSION)"
	@echo ""
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-22s %s\n", $$1, $$2}'

version: ## Show current version
	@echo "mycelium v$(VERSION)"

status: ## Show configuration and installation status
	@echo "Configuration:"
	@echo "  Version:     $(VERSION)"
	@echo "  Install dir: $(INSTALL_DIR)"
	@echo ""
	@echo "Installed binaries:"
	@ls -la "$(INSTALL_DIR)/$(BINARY_DAEMON)" 2>/dev/null || echo "  $(BINARY_DAEMON): not installed"
	@ls -la "$(INSTALL_DIR)/$(BINARY_PRIVATE)" 2>/dev/null || echo "  $(BINARY_PRIVATE): not installed"

# ============================================================================
# Development
# ============================================================================

build: build-daemon ## Build the mycelium daemon (release)

build-daemon: ## Build mycelium daemon binary (release)
	$(CARGO_ENV) cargo build --release --manifest-path myceliumd/Cargo.toml

build-private: ## Build mycelium-private daemon binary (release)
	$(CARGO_ENV) cargo build --release --manifest-path myceliumd-private/Cargo.toml

build-all: build-daemon build-private ## Build all daemon binaries (release)

dev: ## Run mycelium daemon in development mode with debug logging
	$(CARGO_ENV) RUST_LOG=debug cargo run --manifest-path myceliumd/Cargo.toml

MYCELIUM_PEERS := \
	tcp://188.40.132.242:9651 tcp://136.243.47.186:9651 \
	tcp://185.69.166.7:9651 tcp://185.69.166.8:9651 \
	tcp://65.21.231.58:9651 tcp://65.109.18.113:9651 \
	tcp://209.159.146.190:9651 tcp://5.78.122.16:9651 \
	tcp://5.223.43.251:9651 tcp://142.93.217.194:9651 \
	quic://188.40.132.242:9651 quic://136.243.47.186:9651 \
	quic://185.69.166.7:9651 quic://185.69.166.8:9651 \
	quic://65.21.231.58:9651 quic://65.109.18.113:9651 \
	quic://209.159.146.190:9651 quic://5.78.122.16:9651 \
	quic://5.223.43.251:9651 quic://142.93.217.194:9651

MYCELIUM_CMD := "$(INSTALL_DIR)/$(BINARY_DAEMON)" --peers $(MYCELIUM_PEERS) --tun-name utun9

SCREEN_NAME := mycelium

run: install ## Build, install, and run the mycelium daemon in a screen session
	@if screen -list | grep -q "$(SCREEN_NAME)"; then \
		echo "Mycelium screen session found, stopping..."; \
		sudo pkill -f "$(BINARY_DAEMON)" 2>/dev/null || true; \
		screen -S $(SCREEN_NAME) -X quit 2>/dev/null || true; \
		sleep 1; \
	fi
	@echo "Starting mycelium in screen session '$(SCREEN_NAME)'..."
	@if [ "$$(id -u)" -ne 0 ]; then \
		sudo screen -dmS $(SCREEN_NAME) $(MYCELIUM_CMD); \
	else \
		screen -dmS $(SCREEN_NAME) $(MYCELIUM_CMD); \
	fi
	@echo "Mycelium running in screen '$(SCREEN_NAME)'. Use 'sudo screen -r $(SCREEN_NAME)' to attach."

stop: ## Stop the mycelium daemon and kill the screen session
	@sudo pkill -f "$(BINARY_DAEMON)" 2>/dev/null || true
	@sudo screen -S $(SCREEN_NAME) -X quit 2>/dev/null || true
	@echo "Mycelium stopped."

restart: install stop run ## Rebuild, stop, and restart the mycelium daemon

check: ## Fast code check across all workspace crates
	$(CARGO_ENV) cargo check
	$(CARGO_ENV) cargo check --manifest-path myceliumd/Cargo.toml
	$(CARGO_ENV) cargo check --manifest-path myceliumd-private/Cargo.toml

fmt: ## Format all code
	$(CARGO_ENV) cargo fmt
	$(CARGO_ENV) cargo fmt --manifest-path myceliumd/Cargo.toml
	$(CARGO_ENV) cargo fmt --manifest-path myceliumd-private/Cargo.toml

fmt-check: ## Check formatting without modifying
	$(CARGO_ENV) cargo fmt --check
	$(CARGO_ENV) cargo fmt --check --manifest-path myceliumd/Cargo.toml
	$(CARGO_ENV) cargo fmt --check --manifest-path myceliumd-private/Cargo.toml

lint: ## Run clippy on all workspace crates
	$(CARGO_ENV) cargo clippy --all-targets -- -D warnings
	$(CARGO_ENV) cargo clippy --all-targets --manifest-path myceliumd/Cargo.toml -- -D warnings
	$(CARGO_ENV) cargo clippy --all-targets --manifest-path myceliumd-private/Cargo.toml -- -D warnings

# ============================================================================
# Testing
# ============================================================================

test: ## Run all workspace unit tests
	$(CARGO_ENV) cargo test --lib
	$(CARGO_ENV) cargo test --lib --manifest-path myceliumd/Cargo.toml
	$(CARGO_ENV) cargo test --lib --manifest-path myceliumd-private/Cargo.toml

# ============================================================================
# Deployment
# ============================================================================

install: install-daemon ## Build and install mycelium daemon to ~/hero/bin

install-daemon: build-daemon ## Build and install mycelium daemon to ~/hero/bin
	@source $(BUILD_LIB) && install_binaries "$(DAEMON_RELEASE_DIR)" "$(BINARY_DAEMON)"

install-private: build-private ## Build and install mycelium-private daemon to ~/hero/bin
	@source $(BUILD_LIB) && install_binaries "$(PRIVATE_RELEASE_DIR)" "$(BINARY_PRIVATE)"

install-all: install-daemon install-private ## Build and install all daemon binaries to ~/hero/bin

installdev: installdev-daemon ## Build dev mode and install mycelium daemon (fastest compile)

installdev-daemon: ## Build dev mode and install mycelium daemon (fastest compile)
	$(CARGO_ENV) cargo build --manifest-path myceliumd/Cargo.toml
	@source $(BUILD_LIB) && install_binaries "$(DAEMON_DEBUG_DIR)" "$(BINARY_DAEMON)"

installdev-private: ## Build dev mode and install mycelium-private daemon (fastest compile)
	$(CARGO_ENV) cargo build --manifest-path myceliumd-private/Cargo.toml
	@source $(BUILD_LIB) && install_binaries "$(PRIVATE_DEBUG_DIR)" "$(BINARY_PRIVATE)"

clean: ## Remove all build artifacts
	$(CARGO_ENV) cargo clean
	$(CARGO_ENV) cargo clean --manifest-path myceliumd/Cargo.toml
	$(CARGO_ENV) cargo clean --manifest-path myceliumd-private/Cargo.toml

all: clean build-all test ## Full build and test cycle

# ============================================================================
# Maintenance
# ============================================================================

update-deps: ## Update all dependencies
	$(CARGO_ENV) cargo update
	$(CARGO_ENV) cargo update --manifest-path myceliumd/Cargo.toml
	$(CARGO_ENV) cargo update --manifest-path myceliumd-private/Cargo.toml
