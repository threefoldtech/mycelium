#!/usr/bin/env bash
# =============================================================================
# build_lib.sh — Shared build functions for Hero projects
#
# Usage:
#   source scripts/build_lib.sh
#
# Provides reusable functions for building, verifying, installing, and
# publishing Hero project binaries. Used by both the Makefile and CI workflows.
#
# Auto-loads buildenv.sh from repository root automatically.
# =============================================================================

# ---------------------------------------------------------------------------
# Clean up all variables that may be set by this file or previous runs
# This ensures a clean state and prevents leftover values from affecting builds
# ---------------------------------------------------------------------------
unset REPO_SOURCE
unset PROJECT_NAME
unset BINARIES
unset CARGO_PACKAGE
unset ALL_FEATURES
unset VERSION
unset PATCHLEVEL
unset PORTS
unset FORGE_PACKAGE_NAME
unset INSTALL_DIR
unset BIN_DIR
unset SERVER
unset OWNER
unset DOCKER_LINUX_BUILD
unset FORGEJO_INSTANCE
unset FORGEJO_RUNNER_MIN_VERSION
unset FORGEJO_RUNNER_VERSION

# ---------------------------------------------------------------------------
# Auto-detect repository root and source buildenv.sh
# ---------------------------------------------------------------------------

# Find repository root by looking for .git directory
_find_repo_root() {
    local current_dir="$PWD"

    while [ "$current_dir" != "/" ]; do
        if [ -d "$current_dir/.git" ]; then
            echo "$current_dir"
            return 0
        fi
        current_dir="$(dirname "$current_dir")"
    done

    echo "ERROR: build_lib.sh: Cannot find repository root (.git directory)" >&2
    echo "  Please run from within a git repository" >&2
    return 1
}

# Export REPO_SOURCE (repository root)
if [ -z "${REPO_SOURCE:-}" ]; then
    REPO_SOURCE="$(_find_repo_root)" || return 1
    export REPO_SOURCE
fi

# Auto-source buildenv.sh if not already sourced
if [ -z "${PROJECT_NAME:-}" ]; then
    if [ -f "$REPO_SOURCE/buildenv.sh" ]; then
        # shellcheck disable=SC1090
        source "$REPO_SOURCE/buildenv.sh" || return 1
    else
        echo "ERROR: build_lib.sh: buildenv.sh not found at $REPO_SOURCE/buildenv.sh" >&2
        return 1
    fi
fi

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

# Validate required environment variables
if [ -z "${PROJECT_NAME:-}" ]; then
    echo "ERROR: build_lib.sh: PROJECT_NAME not set after sourcing buildenv.sh" >&2
    return 1
fi

if [ -z "${BINARIES:-}" ]; then
    echo "ERROR: build_lib.sh: BINARIES not set in buildenv.sh" >&2
    return 1
fi

if [ -z "${ALL_FEATURES:-}" ]; then
    echo "ERROR: build_lib.sh: ALL_FEATURES not set in buildenv.sh" >&2
    return 1
fi

# Use PROJECT_NAME for FORGE_PACKAGE_NAME if not already set
FORGE_PACKAGE_NAME="${FORGE_PACKAGE_NAME:-$PROJECT_NAME}"

# Set INSTALL_DIR with default fallback
INSTALL_DIR="${INSTALL_DIR:-$HOME/hero/bin}"

# ---------------------------------------------------------------------------
# get_port — Get port by index from PORTS configuration
#
# Returns the Nth port from PORTS list (1-indexed).
# If no index given, returns the primary (first) port.
#
# Usage:
#   PORT=$(get_port)       # Primary port (first)
#   PORT=$(get_port 1)     # First port
#   PORT=$(get_port 2)     # Second port
#   PORT=$(get_port 3)     # Third port
#
# Arguments:
#   INDEX   - Port index (1-indexed), default: 1 (primary)
#
# Examples:
#   get_port              # Returns: 3322
#   get_port 1            # Returns: 3322
#   get_port 2            # Returns: 3323
# ---------------------------------------------------------------------------
get_port() {
    local index="${1:-1}"

    # Get Nth port (1-indexed)
    echo "$PORTS" | awk "{print \$$index}"
}

# ---------------------------------------------------------------------------
# forge_config — Extract SERVER and OWNER from git config
#
# Extracts Forge server URL and repository owner from .git/config.
# Parses remote.origin.url and exports as SERVER and OWNER variables.
# Fails if git config cannot be read or parsed.
#
# Usage:
#   forge_config
#   echo "OWNER=$OWNER, SERVER=$SERVER"
#
# Examples:
#   • Git URL: https://forge.ourworld.tf/geomind_code/hero_redis.git
#     → SERVER=https://forge.ourworld.tf, OWNER=geomind_code
#   • Git URL: git@forge.ourworld.tf:geomind_code/hero_redis.git
#     → SERVER=https://forge.ourworld.tf, OWNER=geomind_code
#
# Error Cases:
#   • Git URL not found in .git/config
#   • Cannot parse SERVER from git URL
#   • Cannot parse OWNER from git URL
# ---------------------------------------------------------------------------
forge_config() {
    # Get remote.origin.url from git config
    local git_url
    git_url=$(git config --get remote.origin.url 2>/dev/null)

    if [ -z "$git_url" ]; then
        echo "ERROR: forge_config: Cannot read remote.origin.url from .git/config" >&2
        return 1
    fi

    # Parse git URL to extract host and owner
    local server owner

    # Extract host (works for both https:// and git@ URLs)
    # Use sed for better compatibility across bash versions
    if echo "$git_url" | grep -qE "^(https://|git@)"; then
        # Extract domain/host from URL
        if echo "$git_url" | grep -q "^https://"; then
            # https://forge.ourworld.tf/...
            server=$(echo "$git_url" | sed -E 's|^https://([^/]+)/.*|\1|')
            server="https://$server"
        elif echo "$git_url" | grep -q "^git@"; then
            # git@forge.ourworld.tf:...
            server=$(echo "$git_url" | sed -E 's|^git@([^:]+):.*|\1|')
            server="https://$server"
        fi
    else
        echo "ERROR: forge_config: Cannot parse SERVER from git URL: $git_url" >&2
        return 1
    fi

    # Validate SERVER was actually parsed (not just protocol)
    if [ -z "$server" ] || [ "$server" = "https://" ]; then
        echo "ERROR: forge_config: Failed to extract valid SERVER from git URL: $git_url" >&2
        return 1
    fi

    # Extract owner (path component before repo name)
    # git@host:owner/repo.git or https://host/owner/repo.git
    # First normalize: convert git@ URLs to https format for consistent parsing
    local normalized_url="$git_url"
    if echo "$git_url" | grep -q "^git@"; then
        # git@forge.ourworld.tf:geomind_code/hero_redis.git -> https://forge.ourworld.tf/geomind_code/hero_redis.git
        normalized_url=$(echo "$git_url" | sed -E 's|^git@([^:]+):(.*)$|https://\1/\2|')
    fi
    # Now extract owner from normalized URL: https://host/owner/repo.git
    owner=$(echo "$normalized_url" | sed -E 's|^https://[^/]+/([^/]+)/.*|\1|')

    if [ -z "$owner" ]; then
        echo "ERROR: forge_config: Cannot parse OWNER from git URL: $git_url" >&2
        return 1
    fi

    # Validate OWNER was actually parsed
    if [ -z "$owner" ]; then
        echo "ERROR: forge_config: Failed to extract valid OWNER from git URL: $git_url" >&2
        return 1
    fi

    # Final validation: SERVER should contain a domain
    if ! [[ "$server" =~ \. ]]; then
        echo "ERROR: forge_config: SERVER does not look like a valid domain: $server" >&2
        return 1
    fi

    # Export parsed values
    export SERVER="$server"
    export OWNER="$owner"
}

# ---------------------------------------------------------------------------
# runner_setup — Install Forgejo Actions runner binary
#
# Downloads and installs the forgejo-runner binary.
# Does not require any tokens - just installs the executable.
# Optionally creates system user on Linux.
#
# Usage:
#   runner_setup
#
# Environment Variables (optional):
#   DOCKER_LINUX_BUILD - If set to 1, enables Docker check (Linux only)
#
# Examples:
#   runner_setup                    # Install binary
#   DOCKER_LINUX_BUILD=1 runner_setup  # Check Docker availability
#
# Notes:
#   • Requires curl for downloading binary
#   • Installs to /usr/local/bin by default
#   • Creates 'runner' system user on Linux (requires sudo)
#   • On macOS, runner will run as current user
# ---------------------------------------------------------------------------
runner_setup() {
    echo "--- Installing Forgejo Actions runner binary ---"

    # Check if forgejo-runner already exists
    if command -v forgejo-runner &>/dev/null; then
        echo "✓ forgejo-runner already installed at $(command -v forgejo-runner)"
        return 0
    fi

    # Check if correct version is installed
    if _check_forgejo_runner_version; then
        echo "✓ forgejo-runner v14.0.2 confirmed"
    else
        echo "Installing/updating forgejo-runner to v14.0.2..."
        if ! _install_forgejo_runner_binary; then
            echo "Binary download failed, falling back to building from source..."
            if ! _build_forgejo_runner_from_source main; then
                echo "ERROR: Failed to install forgejo-runner (both binary download and source build failed)" >&2
                return 1
            fi
        fi
    fi

    # Check Docker if enabled (Linux-specific)
    if [ "${DOCKER_LINUX_BUILD:-0}" = "1" ]; then
        if command -v docker &>/dev/null; then
            echo "✓ Docker available - Linux builds can use containers"
        else
            echo "⚠ DOCKER_LINUX_BUILD set but docker not found - install docker first"
        fi
    fi

    # Create runner user only on Linux (not needed on macOS)
    if [[ "$OSTYPE" == "darwin"* ]]; then
        echo "✓ macOS detected - runner will run as current user: $USER"
    else
        # On Linux, create runner system user
        if ! id "runner" &>/dev/null 2>&1; then
            echo "Creating 'runner' system user..."
            if ! useradd -m -s /bin/bash runner 2>/dev/null; then
                echo "ERROR: runner_setup: Failed to create 'runner' system user" >&2
                echo "  This step requires sudo privileges" >&2
                return 1
            fi
        fi

        # Verify runner user was created
        if ! id "runner" &>/dev/null 2>&1; then
            echo "ERROR: runner_setup: 'runner' user not found after creation" >&2
            return 1
        fi

        # Add runner to docker group if docker is available
        if command -v docker &>/dev/null; then
            if ! usermod -aG docker runner 2>/dev/null; then
                echo "⚠ Warning: Could not add runner to docker group (may need sudo)" >&2
            else
                echo "✓ Added runner user to docker group"
            fi
        fi

        echo "✓ Linux detected - runner will run as system user: runner"
    fi

    echo "✓ Forgejo runner binary installed and ready for registration"
}

# ---------------------------------------------------------------------------
# runner_register — Register Forgejo Actions runner with instance
#
# Registers the forgejo-runner binary with a Forgejo instance.
# Requires a registration token obtained from Forgejo admin panel.
#
# Usage:
#   runner_register REGISTRATION_TOKEN [RUNNER_NAME] [RUNNER_LABELS]
#
# Arguments:
#   REGISTRATION_TOKEN  - Token from Forgejo admin → Actions → Runners (required)
#   RUNNER_NAME         - Name for this runner (default: hostname)
#   RUNNER_LABELS       - Comma-separated labels (default: macos/linux)
#
# Environment Variables (required):
#   PROJECT_NAME        - From buildenv.sh (used for forge_config)
#
# Examples:
#   runner_register "token_abc123"
#   runner_register "token_abc123" "my-runner" "macos,docker"
#
# Notes:
#   • Requires forgejo-runner binary (install with runner_setup first)
#   • Extracts instance URL from git config via forge_config()
#   • On macOS: runs as current user (no sudo needed)
#   • On Linux: runs as 'runner' system user (with sudo)
#   • Creates ~/.forgejo/runner/config.yaml after registration
# ---------------------------------------------------------------------------
runner_register() {
    local registration_token="$1"
    local runner_name="${2:-$(hostname || echo 'local-runner')}"
    local runner_labels="$3"
    local docker_available=false

    echo "--- Registering Forgejo Actions runner ---"

    # Validate arguments
    if [ -z "$registration_token" ]; then
        echo "ERROR: runner_register: REGISTRATION_TOKEN argument required" >&2
        echo "  Get token from: Forgejo admin → Actions → Runners" >&2
        echo "  Usage: runner_register REGISTRATION_TOKEN [RUNNER_NAME] [RUNNER_LABELS]" >&2
        return 1
    fi

    # Validate buildenv.sh was sourced (PROJECT_NAME should be set)
    if [ -z "${PROJECT_NAME:-}" ]; then
        echo "ERROR: runner_register: buildenv.sh not sourced" >&2
        echo "  Run: source buildenv.sh && runner_register TOKEN" >&2
        return 1
    fi

    # Get instance URL and owner from git config
    forge_config
    local instance_url="$SERVER"

    # Validate forge_config extracted values
    if [ -z "$instance_url" ] || [ "$instance_url" = "https://" ]; then
        echo "ERROR: runner_register: Cannot extract instance URL from git config" >&2
        echo "  git remote.origin.url: $(git config --get remote.origin.url)" >&2
        return 1
    fi

    # Verify forgejo-runner is installed
    if ! command -v forgejo-runner &>/dev/null; then
        echo "ERROR: runner_register: forgejo-runner not found" >&2
        echo "  Install first: runner_setup" >&2
        return 1
    fi

    # Setup default labels if not provided
    if [ -z "$runner_labels" ]; then
        # Default labels based on OS
        if [[ "$OSTYPE" == "darwin"* ]]; then
            runner_labels="macos"
        else
            runner_labels="linux"
        fi
    fi

    # Register runner with Forgejo
    echo "Registering runner with Forgejo instance..."
    if [[ "$OSTYPE" == "darwin"* ]]; then
        # On macOS, register as current user (no sudo needed)
        if ! forgejo-runner register \
            --instance "$instance_url" \
            --token "$registration_token" \
            --name "$runner_name" \
            --labels "$runner_labels" \
            --no-interactive; then
            echo "ERROR: Failed to register runner with Forgejo" >&2
            echo "  Instance: $instance_url" >&2
            echo "  Name: $runner_name" >&2
            echo "  Labels: $runner_labels" >&2
            echo "  Check registration token validity" >&2
            return 1
        fi
    else
        # On Linux, register as runner user (with sudo)
        if ! sudo -u runner forgejo-runner register \
            --instance "$instance_url" \
            --token "$registration_token" \
            --name "$runner_name" \
            --labels "$runner_labels" \
            --no-interactive; then
            echo "ERROR: Failed to register runner with Forgejo" >&2
            echo "  Instance: $instance_url" >&2
            echo "  Name: $runner_name" >&2
            echo "  Labels: $runner_labels" >&2
            echo "  Check registration token validity" >&2
            return 1
        fi
    fi

    echo "✓ Runner registered successfully"
    echo "  Instance: $instance_url"
    echo "  Name: $runner_name"
    echo "  Labels: $runner_labels"
    echo ""
    echo "Next steps:"
    if [[ "$OSTYPE" == "darwin"* ]]; then
        echo "  Start daemon: forgejo-runner daemon"
    else
        echo "  Start daemon: sudo -u runner forgejo-runner daemon"
        echo "  Or configure systemd service for autostart"
    fi
}


# ---------------------------------------------------------------------------
# _check_forgejo_runner_version — Check if forgejo-runner matches required version
#
# Verifies that forgejo-runner is version specified (default: 12.6.3+).
# Can be overridden with FORGEJO_RUNNER_MIN_VERSION environment variable.
# Returns 0 if version OK, 1 if missing or outdated.
# ---------------------------------------------------------------------------
_check_forgejo_runner_version() {
    local required_version="${FORGEJO_RUNNER_MIN_VERSION:-12.6.3}"

    if ! command -v forgejo-runner &>/dev/null; then
        echo "forgejo-runner not installed"
        return 1
    fi

    local current_version
    current_version=$(forgejo-runner --version 2>/dev/null | grep -o "version [^ ]*" | cut -d' ' -f2)

    if [ -z "$current_version" ] || [ "$current_version" = "dev" ]; then
        echo "forgejo-runner version: $current_version (not release version)"
        return 1
    fi

    echo "forgejo-runner version: $current_version"

    # Version check: compare semantic versions
    if [ "$current_version" = "$required_version" ]; then
        return 0
    fi

    # Check if current >= required
    local current_major=$(echo "$current_version" | cut -d'.' -f1)
    local current_minor=$(echo "$current_version" | cut -d'.' -f2)
    local current_patch=$(echo "$current_version" | cut -d'.' -f3)

    local required_major=$(echo "$required_version" | cut -d'.' -f1)
    local required_minor=$(echo "$required_version" | cut -d'.' -f2)
    local required_patch=$(echo "$required_version" | cut -d'.' -f3)

    if [ "$current_major" -gt "$required_major" ] || \
       ([ "$current_major" -eq "$required_major" ] && [ "$current_minor" -gt "$required_minor" ]) || \
       ([ "$current_major" -eq "$required_major" ] && [ "$current_minor" -eq "$required_minor" ] && [ "$current_patch" -ge "$required_patch" ]); then
        return 0
    fi

    echo "ERROR: forgejo-runner version $current_version is older than required $required_version"
    return 1
}

# ---------------------------------------------------------------------------
# _install_forgejo_runner_binary — Download and install latest forgejo-runner
#
# Downloads and installs the latest version of forgejo-runner (v12.6.3+).
# Detects OS and architecture automatically.
# ---------------------------------------------------------------------------
_install_forgejo_runner_binary() {
    local version="${FORGEJO_RUNNER_VERSION:-latest}"
    local arch
    local os

    # Detect OS and architecture
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    local machine=$(uname -m)
    case "$machine" in
        x86_64)  arch="amd64" ;;
        aarch64) arch="arm64" ;;
        arm64)   arch="arm64" ;;  # macOS Apple Silicon
        armv7l)  arch="armv7" ;;
        *)       echo "ERROR: Unsupported architecture $machine"; return 1 ;;
    esac

    # Get latest version if not specified
    if [ "$version" = "latest" ]; then
        echo "Fetching latest forgejo-runner version..."
        local release_info
        release_info=$(curl -sS "https://code.forgejo.org/api/v1/repos/forgejo/runner/releases/latest" 2>/dev/null)
        version=$(echo "$release_info" | grep -o '"tag_name":"[^"]*' | head -1 | cut -d'"' -f4 | sed 's/^v//')
        if [ -z "$version" ]; then
            echo "ERROR: Could not fetch latest Forgejo runner version"
            return 1
        fi
    fi

    # Remove 'v' prefix if present
    version="${version#v}"

    # Construct download URL
    local url="https://code.forgejo.org/forgejo/runner/releases/download/v${version}/forgejo-runner-${version}-${os}-${arch}"

    echo "Downloading forgejo-runner v${version} for ${os}/${arch}..."

    if ! curl -sS -L -o /tmp/forgejo-runner "$url"; then
        echo "ERROR: Failed to download from $url"
        return 1
    fi

    # Validate that downloaded file is actually a binary (not HTML error page)
    if file /tmp/forgejo-runner | grep -q "ASCII text"; then
        echo "ERROR: Downloaded file is not a binary (likely 404 error page)"
        echo "  URL: $url"
        rm -f /tmp/forgejo-runner
        return 1
    fi

    # Ensure INSTALL_DIR exists (default: ~/hero/bin)
    local install_path="${INSTALL_DIR:-$HOME/hero/bin}"

    if [ ! -d "$install_path" ]; then
        echo "Creating INSTALL_DIR: $install_path"
        mkdir -p "$install_path" || {
            echo "ERROR: Could not create $install_path"
            return 1
        }
    fi

    echo "Installing to ${install_path}/forgejo-runner..."
    chmod +x /tmp/forgejo-runner

    if mv /tmp/forgejo-runner "${install_path}/forgejo-runner"; then
        echo "✓ forgejo-runner v${version} installed to ${install_path}"
    else
        echo "ERROR: Could not move binary to ${install_path}"
        return 1
    fi
}

# ---------------------------------------------------------------------------
# _build_forgejo_runner_from_source — Build forgejo-runner from source
#
# Clones (or updates) forgejo-runner repo, checks Go version,
# builds from source, and installs to INSTALL_DIR.
#
# Usage:
#   _build_forgejo_runner_from_source [VERSION]
#
# Arguments:
#   VERSION  - Git tag/branch to build (default: main)
#
# Requirements:
#   - git
#   - go (auto-installed if missing)
#   - make
# ---------------------------------------------------------------------------
_build_forgejo_runner_from_source() {
    local version="${1:-main}"
    local repo_url="https://code.forgejo.org/forgejo/runner"
    local build_dir="/tmp/forgejo-runner-build"
    local install_path="${INSTALL_DIR:-$HOME/hero/bin}"

    echo "--- Building forgejo-runner from source ---"
    echo "Version: $version"
    echo "Build directory: $build_dir"
    echo

    # Ensure INSTALL_DIR exists
    if [ ! -d "$install_path" ]; then
        echo "Creating INSTALL_DIR: $install_path"
        mkdir -p "$install_path" || {
            echo "ERROR: Could not create $install_path"
            return 1
        }
    fi

    # Clone or update repository
    if [ -d "$build_dir/.git" ]; then
        echo "Repository already exists, updating..."
        cd "$build_dir" || return 1
        git fetch origin
        git reset --hard "origin/$version" || {
            echo "ERROR: Could not reset to $version"
            return 1
        }
    else
        echo "Cloning repository..."
        rm -rf "$build_dir"
        if ! git clone "$repo_url" "$build_dir"; then
            echo "ERROR: Failed to clone $repo_url"
            return 1
        fi
        cd "$build_dir" || return 1
        git checkout "$version" || {
            echo "ERROR: Could not checkout $version"
            return 1
        }
    fi

    # Check and install Go if needed
    if ! command -v go &>/dev/null; then
        echo "Go not found, installing..."
        if command -v apt-get &>/dev/null; then
            # Debian/Ubuntu
            sudo apt-get update && sudo apt-get install -y golang-go
        elif command -v brew &>/dev/null; then
            # macOS
            brew install go
        elif command -v yum &>/dev/null; then
            # RHEL/CentOS
            sudo yum install -y golang
        else
            echo "ERROR: Could not install Go. Please install manually."
            return 1
        fi
    fi

    # Verify Go installation
    local go_version
    go_version=$(go version | awk '{print $3}')
    echo "✓ Go version: $go_version"
    echo

    # Build forgejo-runner
    # Clear LDFLAGS to avoid incompatible linker flags from environment
    echo "Building forgejo-runner..."
    unset LDFLAGS
    if ! make build; then
        echo "ERROR: Build failed"
        return 1
    fi

    # Find and install the binary
    local binary_path
    if [ -f "forgejo-runner" ]; then
        binary_path="forgejo-runner"
    elif [ -f "dist/forgejo-runner" ]; then
        binary_path="dist/forgejo-runner"
    else
        echo "ERROR: Could not find forgejo-runner binary after build"
        return 1
    fi

    echo "Installing to ${install_path}/forgejo-runner..."
    chmod +x "$binary_path"
    cp "$binary_path" "${install_path}/forgejo-runner" || {
        echo "ERROR: Failed to install binary"
        return 1
    }

    echo "✓ forgejo-runner built and installed from source"
    echo "  Location: ${install_path}/forgejo-runner"
    echo

    # Show version
    "${install_path}/forgejo-runner" --version || echo "⚠ Could not verify version"
}

# ---------------------------------------------------------------------------
# cargo_env — Ensure cargo/rustup are on PATH
# ---------------------------------------------------------------------------
cargo_env() {
    # shellcheck disable=SC1091
    [ -f "$HOME/.cargo/env" ]  && source "$HOME/.cargo/env"
    # shellcheck disable=SC1091
    [ -f "/root/.cargo/env" ]  && source "/root/.cargo/env"
    # shellcheck disable=SC1091
    [ -f "$HOME/.zprofile" ]   && source "$HOME/.zprofile"
    export PATH="$HOME/.cargo/bin:/root/.cargo/bin:$PATH"
}

# ---------------------------------------------------------------------------
# version_from_file — Read version from VERSION file or git tag
# ---------------------------------------------------------------------------
version_from_file() {
    if [ -f VERSION ]; then
        cat VERSION
    else
        echo "0.0.0"
    fi
}

version_from_ref() {
    local ref="${1:-}"
    local ver="${ref#v}"
    if [ -z "$ver" ] || [ "$ver" = "$ref" ]; then
        echo "dev"
    else
        echo "$ver"
    fi
}

# ---------------------------------------------------------------------------
# sync_cargo_version — Ensure Cargo.toml version matches buildenv.sh VERSION
#
# Validates that version in buildenv.sh matches Cargo.toml [package] version.
# Can optionally sync Cargo.toml to match buildenv.sh if versions differ.
#
# Usage:
#   sync_cargo_version [--fix]
#
# Arguments:
#   --fix   - Update Cargo.toml to match buildenv.sh VERSION (optional)
#
# Environment Variables (required):
#   VERSION  - Source version from buildenv.sh
#
# Returns:
#   0 if versions match, 1 if mismatch (or --fix applied)
#
# Examples:
#   sync_cargo_version              # Check only, fail if mismatch
#   sync_cargo_version --fix        # Update Cargo.toml if needed
# ---------------------------------------------------------------------------
sync_cargo_version() {
    local fix_flag="${1:-}"

    # Validate VERSION is set
    if [ -z "${VERSION:-}" ]; then
        echo "ERROR: sync_cargo_version: VERSION environment variable not set" >&2
        echo "  buildenv.sh must be sourced first" >&2
        return 1
    fi

    # Check if Cargo.toml exists
    if [ ! -f "Cargo.toml" ]; then
        echo "ERROR: sync_cargo_version: Cargo.toml not found in current directory" >&2
        return 1
    fi

    # Extract version from Cargo.toml
    local cargo_version
    cargo_version=$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')

    if [ -z "$cargo_version" ]; then
        echo "ERROR: sync_cargo_version: Could not parse version from Cargo.toml" >&2
        return 1
    fi

    # Check if versions match
    if [ "$cargo_version" = "$VERSION" ]; then
        echo "✓ Version in sync: Cargo.toml = buildenv.sh = $VERSION"
        return 0
    fi

    echo "⚠ Version mismatch:"
    echo "  buildenv.sh: $VERSION"
    echo "  Cargo.toml:  $cargo_version"

    # If --fix flag given, update Cargo.toml
    if [ "$fix_flag" = "--fix" ]; then
        echo "Updating Cargo.toml to version $VERSION..."
        sed -i "" "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" Cargo.toml

        # Verify the update
        local new_cargo_version
        new_cargo_version=$(grep '^version' Cargo.toml | head -1 | sed 's/version = "\(.*\)"/\1/')
        if [ "$new_cargo_version" = "$VERSION" ]; then
            echo "✓ Cargo.toml updated to $VERSION"
            return 0
        else
            echo "ERROR: Failed to update Cargo.toml" >&2
            return 1
        fi
    fi

    echo "ERROR: Version mismatch between buildenv.sh and Cargo.toml" >&2
    echo "  Run: sync_cargo_version --fix" >&2
    return 1
}

# ---------------------------------------------------------------------------
# build — Build binaries
#   $1  features   (default: ALL_FEATURES)
#   $2  target     (optional, e.g. x86_64-unknown-linux-musl)
#   $3  profile    (default: release)
# ---------------------------------------------------------------------------
build() {
    local features="${1:-$ALL_FEATURES}"
    local target="${2:-}"
    local profile="${3:-release}"

    local cmd="cargo build --features ${features}"
    [ "$profile" = "release" ] && cmd+=" --release"
    [ -n "$target" ]           && cmd+=" --target ${target}"

    echo "--- Building (features=$features target=${target:-native} profile=$profile) ---"
    eval "$cmd"
}

# ---------------------------------------------------------------------------
# bin_dir — Print the directory that contains built binaries
#   $1  target   (optional)
#   $2  profile  (default: release)
# ---------------------------------------------------------------------------
bin_dir() {
    local target="${1:-}"
    local profile="${2:-release}"
    local base="${CARGO_TARGET_DIR:-target}"

    if [ -n "$target" ]; then
        echo "${base}/${target}/${profile}"
    else
        echo "${base}/${profile}"
    fi
}


# ---------------------------------------------------------------------------
# verify_binaries — Assert all expected binaries exist
#   $1  bin_dir
#   $2  binaries (optional, default: BINARIES)
# ---------------------------------------------------------------------------
verify_binaries() {
    local dir="$1"
    local bins="${2:-$BINARIES}"

    # Validate required arguments
    if [ -z "$dir" ]; then
        echo "ERROR: verify_binaries: BIN_DIR argument not provided" >&2
        return 1
    fi

    # Validate required environment variables
    if [ -z "${BINARIES:-}" ]; then
        echo "ERROR: verify_binaries: BINARIES environment variable not set" >&2
        echo "  Set BINARIES in buildenv.sh" >&2
        return 1
    fi

    # Verify directory exists
    if [ ! -d "$dir" ]; then
        echo "ERROR: verify_binaries: BIN_DIR does not exist: $dir" >&2
        return 1
    fi

    echo "--- Verifying binaries in ${dir} ---"
    for bin in $bins; do
        if [ ! -f "${dir}/${bin}" ]; then
            echo "ERROR: Binary ${dir}/${bin} not found"
            return 1
        fi
        echo "OK: ${dir}/${bin}"
    done
}


# ---------------------------------------------------------------------------
# verify_binaries — Assert all expected binaries exist
#   $1  bin_dir
#   $2  binaries (optional, default: BINARIES)
# ---------------------------------------------------------------------------
verify_binaries() {
    local dir="$1"
    local bins="${2:-$BINARIES}"

    # Validate required arguments
    if [ -z "$dir" ]; then
        echo "ERROR: verify_binaries: BIN_DIR argument not provided" >&2
        return 1
    fi

    # Validate required environment variables
    if [ -z "${BINARIES:-}" ]; then
        echo "ERROR: verify_binaries: BINARIES environment variable not set" >&2
        echo "  Set BINARIES in buildenv.sh" >&2
        return 1
    fi

    # Verify directory exists
    if [ ! -d "$dir" ]; then
        echo "ERROR: verify_binaries: BIN_DIR does not exist: $dir" >&2
        return 1
    fi

    echo "--- Verifying binaries in ${dir} ---"
    for bin in $bins; do
        if [ ! -f "${dir}/${bin}" ]; then
            echo "ERROR: Binary ${dir}/${bin} not found"
            return 1
        fi
        echo "OK: ${dir}/${bin}"
    done
}

# ---------------------------------------------------------------------------
# install_binaries — Copy binaries to INSTALL_DIR
#   $1  bin_dir
#   $2  binaries (optional, default: BINARIES)
# ---------------------------------------------------------------------------
install_binaries() {
    local dir="$1"
    local bins="${2:-$BINARIES}"
    local dest="${INSTALL_DIR}"

    # Validate required arguments
    if [ -z "$dir" ]; then
        echo "ERROR: install_binaries: BIN_DIR argument not provided" >&2
        return 1
    fi

    # Validate required environment variables
    if [ -z "${BINARIES:-}" ]; then
        echo "ERROR: install_binaries: BINARIES environment variable not set" >&2
        echo "  Set BINARIES in buildenv.sh" >&2
        return 1
    fi

    if [ -z "${INSTALL_DIR:-}" ]; then
        echo "ERROR: install_binaries: INSTALL_DIR environment variable not set" >&2
        echo "  Set INSTALL_DIR in buildenv.sh or manually" >&2
        return 1
    fi

    # Verify directory exists
    if [ ! -d "$dir" ]; then
        echo "ERROR: install_binaries: BIN_DIR does not exist: $dir" >&2
        return 1
    fi

    mkdir -p "$dest"
    for bin in $bins; do
        if [ -f "${dir}/${bin}" ]; then
            [ -f "${dest}/${bin}" ] && rm -f "${dest}/${bin}"
            cp "${dir}/${bin}" "${dest}/"
            chmod +x "${dest}/${bin}"
            echo "Installed ${bin} -> ${dest}/${bin}"
        fi
    done
}

# ---------------------------------------------------------------------------
# compress_binaries — UPX-compress binaries in place
#   $1  bin_dir
#   $2  binaries (optional, default: BINARIES)
# ---------------------------------------------------------------------------
compress_binaries() {
    local dir="$1"
    local bins="${2:-$BINARIES}"

    # Validate required arguments
    if [ -z "$dir" ]; then
        echo "ERROR: compress_binaries: BIN_DIR argument not provided" >&2
        return 1
    fi

    # Validate required environment variables
    if [ -z "${BINARIES:-}" ]; then
        echo "ERROR: compress_binaries: BINARIES environment variable not set" >&2
        echo "  Set BINARIES in buildenv.sh" >&2
        return 1
    fi

    # Verify directory exists
    if [ ! -d "$dir" ]; then
        echo "ERROR: compress_binaries: BIN_DIR does not exist: $dir" >&2
        return 1
    fi

    # Skip compression on macOS (UPX has limited support)
    if [[ "$OSTYPE" == "darwin"* ]]; then
        echo "Skipping compression on macOS (UPX not recommended)"
        return 0
    fi

    if ! command -v upx &>/dev/null; then
        echo "Installing UPX..."
        apt-get update && apt-get install -y upx-ucl || apt-get install -y upx
    fi

    for bin in $bins; do
        local path="${dir}/${bin}"
        local size_before
        size_before=$(stat -c%s "$path" 2>/dev/null || stat -f%z "$path")
        echo "Compressing ${bin} (${size_before} bytes)..."
        upx --best --lzma "$path" 2>/dev/null || upx --best "$path" 2>/dev/null || {
            echo "  UPX failed for ${bin}, keeping uncompressed"
            continue
        }
        local size_after
        size_after=$(stat -c%s "$path" 2>/dev/null || stat -f%z "$path")
        echo "  ${size_before} -> ${size_after} bytes ($(( size_after * 100 / size_before ))%)"
    done
}

# ---------------------------------------------------------------------------
# publish_binaries — Upload binaries to Forge generic package registry
#
# Uploads binaries to Forgejo generic package registry. All required
# configuration comes from environment variables (set by buildenv.sh,
# build_for_target(), and Forgejo Actions secrets).
#
# Usage:
#   publish_binaries ARTIFACT_SUFFIX
#
# Arguments:
#   ARTIFACT_SUFFIX   - e.g. linux-amd64, darwin-arm64 (required)
#
# Environment Variables (all required, set by buildenv.sh, build_for_target(), or Forgejo):
#   BIN_DIR           - Directory containing binaries (set by build_for_target)
#   VERSION           - Version to publish (from buildenv.sh or VERSION file)
#   FORGEJO_TOKEN             - Forgejo API token (from workflow secrets)
#   FORGE_PACKAGE_NAME - Package name (from buildenv.sh, defaults to PROJECT_NAME)
#   BINARIES          - Space-separated binary names (from buildenv.sh)
#   SERVER            - Forge server URL (from buildenv.sh, default: https://forge.ourworld.tf)
#   OWNER             - Repository owner (from buildenv.sh, auto-detected or default: geomind_code)
#
# Examples:
#   publish_binaries "linux-amd64"
#   publish_binaries "darwin-arm64"
#
# Error Handling:
#   Fails with clear error messages if any required environment variable is missing.
#   Validates binaries exist before attempting upload.
# ---------------------------------------------------------------------------
publish_binaries() {
    local suffix="$1"

    # Validate BIN_DIR environment variable
    if [ -z "${BIN_DIR:-}" ]; then
        echo "ERROR: publish_binaries: BIN_DIR environment variable not set" >&2
        echo "  Set by build_for_target() or manually: export BIN_DIR=<path>" >&2
        return 1
    fi

    if [ -z "$suffix" ]; then
        echo "ERROR: publish_binaries: ARTIFACT_SUFFIX argument not provided" >&2
        echo "  Pass artifact suffix like: linux-amd64, darwin-arm64, linux-arm64" >&2
        return 1
    fi

    # Validate suffix format (must contain at least os-arch pattern)
    if ! [[ "$suffix" =~ ^[a-z0-9]+-[a-z0-9]+$ ]]; then
        echo "ERROR: publish_binaries: ARTIFACT_SUFFIX has invalid format: $suffix" >&2
        echo "  Expected format: os-arch (e.g., linux-amd64, darwin-arm64)" >&2
        return 1
    fi

    # Validate all required environment variables
    if [ -z "${VERSION:-}" ]; then
        echo "ERROR: publish_binaries: VERSION environment variable not set" >&2
        echo "  Set VERSION in buildenv.sh or VERSION file" >&2
        return 1
    fi

    if [ -z "${FORGEJO_TOKEN:-}" ]; then
        echo "ERROR: publish_binaries: FORGEJO_TOKEN environment variable not set" >&2
        echo "  Configure FORGEJO_TOKEN in Forgejo workflow secrets" >&2
        return 1
    fi

    if [ -z "${FORGE_PACKAGE_NAME:-}" ]; then
        echo "ERROR: publish_binaries: FORGE_PACKAGE_NAME environment variable not set" >&2
        echo "  Set FORGE_PACKAGE_NAME in buildenv.sh or it will default to PROJECT_NAME" >&2
        return 1
    fi

    if [ -z "${BINARIES:-}" ]; then
        echo "ERROR: publish_binaries: BINARIES environment variable not set" >&2
        echo "  Set BINARIES in buildenv.sh" >&2
        return 1
    fi

    if [ -z "${SERVER:-}" ]; then
        echo "ERROR: publish_binaries: SERVER environment variable not set" >&2
        echo "  Set SERVER via forge_config() or in buildenv.sh" >&2
        return 1
    fi

    if [ -z "${OWNER:-}" ]; then
        echo "ERROR: publish_binaries: OWNER environment variable not set" >&2
        echo "  Set OWNER via forge_config() or in buildenv.sh" >&2
        return 1
    fi

    # Verify directory exists
    if [ ! -d "$BIN_DIR" ]; then
        echo "ERROR: publish_binaries: BIN_DIR does not exist: $BIN_DIR" >&2
        return 1
    fi

    # Verify all binaries exist before attempting upload
    for bin in $BINARIES; do
        if [ ! -f "${BIN_DIR}/${bin}" ]; then
            echo "ERROR: publish_binaries: Binary not found: ${BIN_DIR}/${bin}" >&2
            return 1
        fi
    done

    echo "--- Publishing to ${SERVER} (version=${VERSION}) ---"
    for bin in $BINARIES; do
        local artifact="${bin}-${suffix}"
        local url="${SERVER}/api/crates/${OWNER}/generic/${FORGE_PACKAGE_NAME}/${VERSION}/${artifact}"

        echo "Deleting existing ${artifact} (if any)..."
        curl -sS -X DELETE -H "Authorization: token ${FORGEJO_TOKEN}" "$url" || true

        echo "Uploading ${BIN_DIR}/${bin} -> ${artifact}"
        curl -X PUT -H "Authorization: token ${FORGEJO_TOKEN}" \
            --upload-file "${BIN_DIR}/${bin}" --fail --show-error "$url"
        echo "  Published: ${artifact}"
    done
}

# ---------------------------------------------------------------------------
# check_clean_tree — Fail if there are uncommitted or unpushed changes
# ---------------------------------------------------------------------------
check_clean_tree() {
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "ERROR: Uncommitted changes. Commit and push before shipping."
        git status --short
        return 1
    fi
    if [ "$(git rev-list '@{u}..HEAD' 2>/dev/null | wc -l)" -gt 0 ]; then
        echo "ERROR: Unpushed commits. Run 'git push' first."
        return 1
    fi
}

# ---------------------------------------------------------------------------
# setup_linux_toolchain — Setup Linux build toolchain (musl + cross-compile)
#
# Installs base build tools and optionally cross-compilation toolchain.
# Requires: apt-get available (Docker/Linux environment)
#
# Usage:
#   setup_linux_toolchain [TARGET]
#
# Arguments:
#   TARGET  - Optional cross-compile target (e.g., aarch64-unknown-linux-gnu)
#
# Examples:
#   setup_linux_toolchain                          # Native musl build
#   setup_linux_toolchain aarch64-unknown-linux-gnu  # ARM64 cross-compile
# ---------------------------------------------------------------------------
setup_linux_toolchain() {
    local target="${1:-}"

    # Base build tools
    apt-get update
    apt-get install -y pkg-config build-essential musl-tools

    # Setup Rust components
    cargo_env
    rustup component add rustfmt clippy 2>/dev/null || true

    # Cross-compilation toolchain if target specified and not native musl
    if [ -n "$target" ] && [ "$target" != "x86_64-unknown-linux-musl" ]; then
        rustup target add "$target"

        # Install cross-compiler for ARM64
        if [ "$target" = "aarch64-unknown-linux-gnu" ]; then
            apt-get install -y gcc-aarch64-linux-gnu g++-aarch64-linux-gnu
        fi
    fi
}

# ---------------------------------------------------------------------------
# build_binaries — Build, verify, and optionally compress binaries
#
# Universal function for building binaries. Handles native builds and
# cross-compilation targets. Sets up Rust environment, builds, verifies,
# and optionally compresses binaries.
#
# Usage:
#   build_binaries [TARGET]
#
# Arguments:
#   TARGET  - Target triple (optional, e.g., x86_64-unknown-linux-musl, aarch64-unknown-linux-gnu)
#             If empty, defaults to native build
#
# Outputs:
#   BIN_DIR - Exported, contains path to built binaries directory
#
# Examples:
#   build_binaries                              # Native build
#   build_binaries x86_64-unknown-linux-musl   # Native musl build (Linux)
#   build_binaries aarch64-unknown-linux-gnu   # Cross-compile for ARM64
# ---------------------------------------------------------------------------
build_binaries() {
    local target="${1:-}"

    # Ensure cargo/rustup are on PATH
    cargo_env

    # Set musl compiler for x86_64 musl target
    if [ "$target" = "x86_64-unknown-linux-musl" ]; then
        export CC_x86_64_unknown_linux_musl=musl-gcc
    fi

    # Build with target (if specified)
    if [ -n "$target" ]; then
        build "$ALL_FEATURES" "$target"
    else
        build "$ALL_FEATURES"
    fi

    # Set BIN_DIR for verification and compression
    export BIN_DIR="$(bin_dir "$target")"

    # Verify binaries exist
    verify_binaries "$BIN_DIR"

    # Compress binaries (skipped on macOS automatically)
    compress_binaries "$BIN_DIR"
}

# ---------------------------------------------------------------------------
# build_for_target — Alias for build_binaries (for backward compatibility)
# ---------------------------------------------------------------------------
build_for_target() {
    build_binaries "$@"
}

# ---------------------------------------------------------------------------
# ci_setup_toolchain — Install rustfmt, clippy, and optional cross-compilation deps
#   $1  target  (optional, for cross-compilation)
# ---------------------------------------------------------------------------
ci_setup_toolchain() {
    local target="${1:-}"

    cargo_env
    rustup component add rustfmt clippy 2>/dev/null || true

    if [ -n "$target" ]; then
        rustup target add "$target"
    fi
}

# ---------------------------------------------------------------------------
# ship_binary — Create git tag and push to trigger CI build & publish
#   $1  tag    (e.g. v0.1.3, defaults to PROJECT_NAME version)
# ---------------------------------------------------------------------------
ship_binary() {
    local tag="${1:-}"

    if [ -z "$tag" ]; then
        if [ -f VERSION ]; then
            tag="v$(cat VERSION)"
        else
            echo "ERROR: No tag specified and VERSION file not found"
            return 1
        fi
    fi

    check_clean_tree

    echo "Shipping $PROJECT_NAME $tag"
    echo "  This will create tag $tag and push it to trigger CI."
    echo ""

    if git rev-parse "$tag" >/dev/null 2>&1; then
        echo "ERROR: Tag $tag already exists. Delete it first with:"
        echo "  git tag -d $tag && git push origin :refs/tags/$tag"
        return 1
    fi

    git tag "$tag"
    git push origin "$tag"

    echo ""
    echo "Done. CI will build and publish $PROJECT_NAME to Forge registry."
}

# ---------------------------------------------------------------------------
# release — Create and push a semver release tag
#
# Bumps version and creates an annotated git tag. Requires being on 'main'
# branch with a clean working tree. Automatically pulls latest from origin.
#
# Usage:
#   release [TYPE]
#
# Arguments:
#   TYPE    - Version bump type: major, minor, or patch (default: patch)
#             • major: x.0.0 (breaking changes)
#             • minor: 0.x.0 (new features)
#             • patch: 0.0.x (bug fixes) [DEFAULT]
#
# Examples:
#   release              # Creates next patch version (0.0.x)
#   release patch        # Same as above
#   release minor        # Creates next minor version (0.x.0)
#   release major        # Creates next major version (x.0.0)
# ---------------------------------------------------------------------------
release() {
    local bump_type="${1:-patch}"

    # Validate bump type
    case "$bump_type" in
        patch|minor|major) ;;
        *)
            echo "ERROR: Unknown release type '$bump_type'"
            echo "Usage: release [patch|minor|major]"
            return 1
            ;;
    esac

    # Check branch
    local current_branch
    current_branch=$(git branch --show-current 2>/dev/null)
    if [ "$current_branch" != "main" ]; then
        echo "ERROR: Must be on 'main' branch to create a release"
        echo "  Current branch: $current_branch"
        return 1
    fi

    # Check for uncommitted changes
    if ! git diff-index --quiet HEAD -- 2>/dev/null; then
        echo "ERROR: You have uncommitted changes"
        git status --short
        return 1
    fi

    # Fetch latest
    git pull origin main || return 1

    # Parse current version
    local latest_tag
    latest_tag=$(git describe --tags --abbrev=0 2>/dev/null || echo "v0.0.0")
    local version_parts
    version_parts=$(echo "$latest_tag" | sed 's/v//' | sed 's/\./ /g')
    local major minor patch
    read -r major minor patch <<< "$version_parts"

    # Calculate new version
    local new_tag
    case "$bump_type" in
        patch)
            new_tag="v${major}.${minor}.$((patch + 1))"
            ;;
        minor)
            new_tag="v${major}.$((minor + 1)).0"
            ;;
        major)
            new_tag="v$((major + 1)).0.0"
            ;;
    esac

    # Confirm before tagging
    echo "Release: $PROJECT_NAME"
    echo "  Type:    $bump_type"
    echo "  Current: $latest_tag"
    echo "  New:     $new_tag"
    echo ""
    read -p "Create and push $new_tag? [y/N] " -r confirm

    if [ "$confirm" != "y" ] && [ "$confirm" != "Y" ]; then
        echo "Cancelled"
        exit 0
    fi

    # Create tag and push
    git tag -a "$new_tag" -m "Release $new_tag"
    git push origin "$new_tag"

    echo ""
    echo "✓ Released $new_tag"
}

# ---------------------------------------------------------------------------
# detect_os — Detect current operating system
#
# Returns:
#   linux, darwin
#
# Example:
#   OS=$(detect_os)
# ---------------------------------------------------------------------------
detect_os() {
    case "$(uname -s)" in
        Linux*)  echo "linux" ;;
        Darwin*) echo "darwin" ;;
        *)
            echo "ERROR: detect_os: Unsupported operating system: $(uname -s)" >&2
            return 1
            ;;
    esac
}

# ---------------------------------------------------------------------------
# detect_arch — Detect current CPU architecture
#
# Returns:
#   amd64, arm64
#
# Example:
#   ARCH=$(detect_arch)
# ---------------------------------------------------------------------------
detect_arch() {
    case "$(uname -m)" in
        x86_64)  echo "amd64" ;;
        aarch64) echo "arm64" ;;
        arm64)   echo "arm64" ;;
        *)
            echo "ERROR: detect_arch: Unsupported architecture: $(uname -m)" >&2
            return 1
            ;;
    esac
}

# ---------------------------------------------------------------------------
# detect_artifact — Detect artifact suffix for the current platform
#
# Combines OS and architecture into a standard artifact suffix.
#
# Returns:
#   linux-amd64, linux-arm64, darwin-arm64, etc.
#
# Example:
#   ARTIFACT=$(detect_artifact)
#   publish_binaries "$ARTIFACT"
# ---------------------------------------------------------------------------
detect_artifact() {
    local os arch
    os=$(detect_os) || return 1
    arch=$(detect_arch) || return 1
    echo "${os}-${arch}"
}

# ---------------------------------------------------------------------------
# configure_hero_path — Add ~/hero/bin to user's shell PATH configuration
#
# Detects the user's shell and appends the hero bin directory to the
# appropriate shell rc file (~/.bashrc, ~/.zshrc, ~/.config/fish/config.fish,
# or ~/.profile). Idempotent — skips if already configured.
#
# Usage:
#   configure_hero_path
#
# Examples:
#   configure_hero_path    # Adds export PATH="$HOME/hero/bin:$PATH" to shell rc
# ---------------------------------------------------------------------------
configure_hero_path() {
    local hero_bin="${INSTALL_DIR:-$HOME/hero/bin}"
    local shell_type path_line rc_file

    # Detect shell type
    case "${SHELL:-/bin/bash}" in
        */fish) shell_type="fish" ;;
        */zsh)  shell_type="zsh" ;;
        */bash) shell_type="bash" ;;
        *)      shell_type="unknown" ;;
    esac

    # Set path line and rc file per shell
    case "$shell_type" in
        fish)
            path_line="set -x PATH $hero_bin \$PATH"
            local config_dir="$HOME/.config/fish"
            mkdir -p "$config_dir"
            rc_file="$config_dir/config.fish"
            ;;
        zsh)
            path_line="export PATH=\"$hero_bin:\$PATH\""
            rc_file="$HOME/.zshrc"
            ;;
        bash)
            path_line="export PATH=\"$hero_bin:\$PATH\""
            rc_file="$HOME/.bashrc"
            ;;
        *)
            path_line="export PATH=\"$hero_bin:\$PATH\""
            rc_file="$HOME/.profile"
            ;;
    esac

    # Idempotent — skip if already configured
    if [ -f "$rc_file" ] && grep -q "hero/bin" "$rc_file" 2>/dev/null; then
        echo "PATH already configured in $rc_file"
        return 0
    fi

    # Append to rc file
    {
        echo ""
        echo "# Hero binaries path"
        echo "$path_line"
    } >> "$rc_file"
    echo "Added hero/bin to PATH in $rc_file"
}

# ---------------------------------------------------------------------------
# download_binaries — Download binaries from Forge generic package registry
#
# Downloads project binaries for the current (or specified) platform from
# the Forge generic package registry and installs them into INSTALL_DIR.
#
# Usage:
#   download_binaries [VERSION] [ARTIFACT_SUFFIX]
#
# Arguments:
#   VERSION          - Version to download (default: VERSION from buildenv.sh)
#   ARTIFACT_SUFFIX  - Platform suffix (default: auto-detected via detect_artifact)
#
# Environment Variables:
#   PROJECT_NAME     - Package name on Forge (from buildenv.sh)
#   BINARIES         - Space-separated binary names (from buildenv.sh)
#   INSTALL_DIR      - Where to install (default: ~/hero/bin)
#   SERVER           - Forge server URL (auto-detected from git remote or default)
#   OWNER            - Repository owner (auto-detected from git remote or default)
#
# Examples:
#   download_binaries                           # Latest version, auto-detect platform
#   download_binaries "0.1.0"                   # Specific version
#   download_binaries "0.1.0" "linux-amd64"     # Specific version + platform
# ---------------------------------------------------------------------------
download_binaries() {
    local version="${1:-${VERSION:-}}"
    local suffix="${2:-}"

    # Auto-detect platform if not specified
    if [ -z "$suffix" ]; then
        suffix=$(detect_artifact) || return 1
    fi

    if [ -z "$version" ]; then
        echo "ERROR: download_binaries: VERSION not set" >&2
        echo "  Pass as argument or set VERSION in buildenv.sh" >&2
        return 1
    fi

    if [ -z "${BINARIES:-}" ]; then
        echo "ERROR: download_binaries: BINARIES environment variable not set" >&2
        return 1
    fi

    # Auto-detect server/owner from git remote if available
    local server="${SERVER:-}"
    local owner="${OWNER:-}"
    if [ -z "$server" ] || [ -z "$owner" ]; then
        if [ -d .git ] || [ -d "$REPO_SOURCE/.git" ]; then
            forge_config 2>/dev/null || true
            server="${SERVER:-https://forge.ourworld.tf}"
            owner="${OWNER:-}"
        else
            server="https://forge.ourworld.tf"
        fi
    fi

    if [ -z "$owner" ]; then
        echo "ERROR: download_binaries: OWNER not set and could not be auto-detected" >&2
        echo "  Set OWNER environment variable or run from a git repository" >&2
        return 1
    fi

    local pkg_name="${FORGE_PACKAGE_NAME:-$PROJECT_NAME}"
    local dest="${INSTALL_DIR:-$HOME/hero/bin}"
    mkdir -p "$dest"

    echo "--- Downloading ${pkg_name} v${version} (${suffix}) ---"

    local failed=0
    for bin in $BINARIES; do
        local artifact="${bin}-${suffix}"
        local url="${server}/api/crates/${owner}/generic/${pkg_name}/${version}/${artifact}"

        echo "Downloading ${artifact}..."
        if command -v curl &>/dev/null; then
            if ! curl -L -f -sS -o "${dest}/${bin}" "$url"; then
                echo "ERROR: Failed to download ${artifact} from ${url}" >&2
                failed=1
                continue
            fi
        elif command -v wget &>/dev/null; then
            if ! wget -q -O "${dest}/${bin}" "$url"; then
                echo "ERROR: Failed to download ${artifact} from ${url}" >&2
                failed=1
                continue
            fi
        else
            echo "ERROR: Neither curl nor wget found" >&2
            return 1
        fi

        chmod +x "${dest}/${bin}"
        echo "  Installed: ${dest}/${bin}"
    done

    if [ "$failed" -ne 0 ]; then
        echo "WARNING: Some binaries failed to download" >&2
        return 1
    fi

    echo "--- All binaries installed to ${dest} ---"
}

# ---------------------------------------------------------------------------
# ci_check — Run formatting, build (debug), test, and clippy
#   $1  features  (default: ALL_FEATURES)
# ---------------------------------------------------------------------------
ci_check() {
    local features="${1:-$ALL_FEATURES}"
    cargo_env

    echo "--- Checking formatting ---"
    cargo fmt --check

    echo "--- Building ---"
    cargo build --features "$features"

    echo "--- Running tests ---"
    cargo test --features "$features"

    echo "--- Clippy (warnings only) ---"
    cargo clippy --features "$features" || true
}

# ---------------------------------------------------------------------------
# verify_forgejo_token — Verify Forgejo API token validity
#
# Tests the FORGEJO_TOKEN by making a simple API call to the Forgejo instance.
# Requires curl to be available.
#
# Usage:
#   verify_forgejo_token [SERVER]
#
# Arguments:
#   SERVER  - Forgejo instance URL (default: https://forge.ourworld.tf)
#
# Environment Variables (required):
#   FORGEJO_TOKEN - API token to verify
#
# Returns:
#   0 if token is valid, 1 if invalid or request fails
#
# Examples:
#   verify_forgejo_token                    # Default to forge.ourworld.tf
#   verify_forgejo_token "https://forge.example.com"
# ---------------------------------------------------------------------------
verify_forgejo_token() {
    local server="${1:-https://forge.ourworld.tf}"

    if [ -z "${FORGEJO_TOKEN:-}" ]; then
        echo "ERROR: verify_forgejo_token: FORGEJO_TOKEN environment variable not set" >&2
        return 1
    fi

    echo "Verifying token with $server..."
    if curl -sf -H "Authorization: token ${FORGEJO_TOKEN}" "${server}/api/v1/user" > /dev/null; then
        echo "✓ Token verified"
        return 0
    else
        echo "ERROR: FORGEJO_TOKEN is invalid or expired" >&2
        return 1
    fi
}

# ---------------------------------------------------------------------------
# runner_install_if_needed — Install forgejo-runner binary if not present
#
# Checks if forgejo-runner is installed. If not, downloads and installs
# the binary from Forge generic package registry.
#
# Usage:
#   runner_install_if_needed [FALLBACK_INSTALL_URL]
#
# Arguments:
#   FALLBACK_INSTALL_URL  - Installation script URL (default: from forge)
#
# Environment Variables (optional):
#   INSTALL_DIR  - Where to install binary (default: ~/hero/bin)
#
# Returns:
#   0 if already installed or installation succeeds, 1 on failure
#
# Examples:
#   runner_install_if_needed                    # Use default install script
#   runner_install_if_needed "https://custom.url/install.sh"
# ---------------------------------------------------------------------------
runner_install_if_needed() {
    local install_url="${1:-https://forge.ourworld.tf/api/crates/lhumina_research/generic/forgejo-runner/dev/install.sh}"

    if command -v forgejo-runner &>/dev/null; then
        echo "✓ forgejo-runner already installed at $(command -v forgejo-runner)"
        return 0
    fi

    echo "Installing forgejo-runner..."
    if ! curl -sSL "$install_url" | bash; then
        echo "ERROR: Failed to install forgejo-runner" >&2
        return 1
    fi

    echo "✓ forgejo-runner installed"
    return 0
}

# ---------------------------------------------------------------------------
# run_build_workflow — Execute Forgejo build workflow for current OS
#
# Detects the current operating system and runs the appropriate build workflow
# (build-macos.yaml for Darwin, build-linux.yaml for Linux).
# Requires forgejo-runner to be installed.
#
# Usage:
#   run_build_workflow [WORKFLOW_DIR]
#
# Arguments:
#   WORKFLOW_DIR  - Directory containing workflows (default: .forgejo/workflows)
#
# Environment Variables (required):
#   FORGEJO_TOKEN - Forgejo API token
#   VERSION       - Version to build
#   OWNER         - Repository owner
#
# Environment Variables (optional):
#   FORGEJO_INSTANCE - Forgejo server URL (default: https://forge.ourworld.tf)
#
# Returns:
#   0 on success, 1 on failure or unsupported OS
#
# Examples:
#   run_build_workflow                      # Use default .forgejo/workflows
#   run_build_workflow ".github/workflows"  # Use custom workflow directory
# ---------------------------------------------------------------------------
run_build_workflow() {
    local workflow_dir="${1:-.forgejo/workflows}"

    # Validate required environment variables
    if [ -z "${FORGEJO_TOKEN:-}" ]; then
        echo "ERROR: run_build_workflow: FORGEJO_TOKEN environment variable not set" >&2
        return 1
    fi

    if [ -z "${VERSION:-}" ]; then
        echo "ERROR: run_build_workflow: VERSION environment variable not set" >&2
        return 1
    fi

    if [ -z "${OWNER:-}" ]; then
        echo "ERROR: run_build_workflow: OWNER environment variable not set" >&2
        return 1
    fi

    # Validate forgejo-runner is installed
    if ! command -v forgejo-runner &>/dev/null; then
        echo "ERROR: run_build_workflow: forgejo-runner not found" >&2
        echo "  Install with: runner_install_if_needed" >&2
        return 1
    fi

    # Determine workflow based on OS
    local os job_name workflow_file
    os=$(detect_os) || return 1

    case "$os" in
        darwin)
            job_name="build-macos"
            workflow_file="${workflow_dir}/build-macos.yaml"
            ;;
        linux)
            job_name="build-linux"
            workflow_file="${workflow_dir}/build-linux.yaml"
            ;;
        *)
            echo "ERROR: run_build_workflow: Unsupported OS: $os" >&2
            return 1
            ;;
    esac

    # Verify workflow file exists
    if [ ! -f "$workflow_file" ]; then
        echo "ERROR: run_build_workflow: Workflow file not found: $workflow_file" >&2
        return 1
    fi

    local forgejo_instance="${FORGEJO_INSTANCE:-https://forge.ourworld.tf}"

    echo "Running build workflow for $os..."
    echo "  Job: $job_name"
    echo "  Version: $VERSION"
    echo "  Owner: $OWNER"
    echo ""

    # Execute the workflow
    forgejo-runner exec \
        -j "$job_name" \
        -s FORGEJO_TOKEN="$FORGEJO_TOKEN" \
        --forgejo-instance "$forgejo_instance" \
        --env VERSION="$VERSION" \
        --env OWNER="$OWNER" \
        -i "-self-hosted" \
        -W "$workflow_file"
}

# ---------------------------------------------------------------------------
# build_package — Run local Forgejo Actions build workflow
#
# Complete workflow for building locally using Forgejo Actions runner:
# 1. Validates FORGEJO_TOKEN is set
# 2. Sources buildenv.sh to get VERSION, PROJECT_NAME, etc.
# 3. Ensures Cargo.toml version matches buildenv.sh VERSION
# 4. Verifies FORGEJO_TOKEN is valid
# 5. Installs forgejo-runner if needed
# 6. Executes platform-appropriate build workflow
#
# Usage:
#   build_package
#
# Environment Variables (required):
#   FORGEJO_TOKEN - Forgejo API token for authentication
#
# Environment Variables (optional):
#   OWNER              - Repository owner (default: geomind_code)
#   FORGEJO_INSTANCE   - Forgejo server URL (default: https://forge.ourworld.tf)
#
# Returns:
#   0 on success, 1 on any failure
#
# Examples:
#   export FORGEJO_TOKEN="..."
#   build_package    # Run local build using Forgejo Actions
#
# Notes:
#   • Must be run from repository root or with buildenv.sh in current directory
#   • Validates version consistency between buildenv.sh and Cargo.toml
#   • Requires curl and bash (for script sourcing)
# ---------------------------------------------------------------------------
build_package() {
    # Validate FORGEJO_TOKEN is set
    if [ -z "${FORGEJO_TOKEN:-}" ]; then
        echo "ERROR: build_package: FORGEJO_TOKEN environment variable not set" >&2
        echo "  Set it with: export FORGEJO_TOKEN='your-token-here'" >&2
        return 1
    fi

    # Source buildenv.sh if not already sourced
    if [ -z "${PROJECT_NAME:-}" ]; then
        if [ -f "buildenv.sh" ]; then
            # shellcheck disable=SC1091
            source ./buildenv.sh
        else
            echo "ERROR: build_package: buildenv.sh not found in current directory" >&2
            return 1
        fi
    fi

    # Set owner with default fallback
    OWNER="${OWNER:-geomind_code}"

    echo "Build version: $VERSION (from buildenv.sh)"

    # Ensure Cargo.toml version matches buildenv.sh VERSION
    sync_cargo_version || return 1

    # Verify token works
    verify_forgejo_token || return 1

    # Check for forgejo-runner and install if needed
    runner_install_if_needed || return 1

    # Run build workflow for current OS
    run_build_workflow || return 1

    echo ""
    echo "Local build complete."
    echo ""
}

# ---------------------------------------------------------------------------
# screen_start — Start a named detached screen session
#
# Usage:
#   screen_start NAME CMD
#
# Arguments:
#   NAME  - Screen session name
#   CMD   - Command to run inside the session
# ---------------------------------------------------------------------------
screen_start() {
    local name="$1"
    local cmd="$2"

    if [ -z "$name" ] || [ -z "$cmd" ]; then
        echo "ERROR: screen_start: NAME and CMD required" >&2
        return 1
    fi

    screen -dmS "$name" bash -c "$cmd"
    echo "✓ Screen session '$name' started"
}

# ---------------------------------------------------------------------------
# screen_stop — Stop a named screen session (idempotent)
#
# Usage:
#   screen_stop NAME
# ---------------------------------------------------------------------------
screen_stop() {
    local name="$1"

    if [ -z "$name" ]; then
        echo "ERROR: screen_stop: NAME required" >&2
        return 1
    fi

    screen -S "$name" -X quit 2>/dev/null || true
}

# ---------------------------------------------------------------------------
# screen_running — Check if a named screen session is running
#
# Returns 0 if session exists, 1 if not.
#
# Usage:
#   screen_running NAME
# ---------------------------------------------------------------------------
screen_running() {
    local name="$1"
    screen -list 2>/dev/null | grep -q "\.$name\s" && return 0 || return 1
}

# ---------------------------------------------------------------------------
# proc_kill — Kill a process by name pattern (graceful then force)
#
# Usage:
#   proc_kill PATTERN
# ---------------------------------------------------------------------------
proc_kill() {
    local pattern="$1"

    if [ -z "$pattern" ]; then
        echo "ERROR: proc_kill: PATTERN required" >&2
        return 1
    fi

    pkill -f "$pattern" 2>/dev/null || true
    sleep 1
    if pgrep -f "$pattern" >/dev/null 2>&1; then
        pkill -9 -f "$pattern" 2>/dev/null || true
    fi
}

# ---------------------------------------------------------------------------
# nc_detect — Detect available nc bridge tool
#
# Prints "ncat" or "socat". Fails with install instructions if neither found.
#
# Usage:
#   TOOL=$(nc_detect)
# ---------------------------------------------------------------------------
nc_detect() {
    if command -v ncat >/dev/null 2>&1; then
        echo "ncat"
        return 0
    fi

    if command -v socat >/dev/null 2>&1; then
        echo "socat"
        return 0
    fi

    echo "ERROR: nc_detect: neither 'ncat' nor 'socat' found" >&2
    echo "  Install on macOS:  brew install nmap" >&2
    echo "  Install on Linux:  apt install ncat  OR  apt install socat" >&2
    return 1
}

# ---------------------------------------------------------------------------
# nc_install — Install ncat if not already present (idempotent)
#
# Usage:
#   nc_install
# ---------------------------------------------------------------------------
nc_install() {
    if command -v ncat >/dev/null 2>&1; then
        echo "✓ ncat already installed"
        return 0
    fi

    if command -v socat >/dev/null 2>&1; then
        echo "✓ socat already installed (will use as nc bridge)"
        return 0
    fi

    echo "Installing ncat..."
    if [[ "$OSTYPE" == "darwin"* ]]; then
        if ! command -v brew >/dev/null 2>&1; then
            echo "ERROR: nc_install: homebrew not found — install from https://brew.sh" >&2
            return 1
        fi
        brew install nmap
    elif command -v apt-get >/dev/null 2>&1; then
        apt-get install -y ncat || apt-get install -y socat
    elif command -v yum >/dev/null 2>&1; then
        yum install -y nmap-ncat || yum install -y socat
    else
        echo "ERROR: nc_install: cannot install ncat — unknown package manager" >&2
        echo "  Please install ncat or socat manually" >&2
        return 1
    fi

    echo "✓ nc tool installed"
}

# ---------------------------------------------------------------------------
# nc_bridge_start — Start a TCP→Unix socket bridge in a screen session
#
# Detects ncat or socat. Uses --keep-open / fork to handle multiple
# concurrent HTTP connections (required for browser access).
#
# Usage:
#   nc_bridge_start SESSION_NAME TCP_PORT UNIX_SOCKET
#
# Arguments:
#   SESSION_NAME  - Screen session name (e.g. hero_proc_ui_nc)
#   TCP_PORT      - TCP port to listen on (e.g. 9999)
#   UNIX_SOCKET   - Unix socket path to forward to
# ---------------------------------------------------------------------------
nc_bridge_start() {
    local session="$1"
    local port="$2"
    local sock="$3"

    if [ -z "$session" ] || [ -z "$port" ] || [ -z "$sock" ]; then
        echo "ERROR: nc_bridge_start: SESSION_NAME TCP_PORT UNIX_SOCKET required" >&2
        return 1
    fi

    local tool
    tool=$(nc_detect) || return 1

    local cmd
    if [ "$tool" = "ncat" ]; then
        cmd="ncat -l 127.0.0.1 $port --keep-open -U $sock"
    else
        cmd="socat TCP-LISTEN:${port},bind=127.0.0.1,reuseaddr,fork UNIX-CLIENT:${sock}"
    fi

    screen_stop "$session"
    screen_start "$session" "$cmd"
    echo "✓ nc bridge started: TCP 127.0.0.1:$port → $sock  (tool=$tool, session=$session)"
}

# ---------------------------------------------------------------------------
# nc_bridge_stop — Stop a nc bridge screen session and release TCP port
#
# Usage:
#   nc_bridge_stop SESSION_NAME TCP_PORT
# ---------------------------------------------------------------------------
nc_bridge_stop() {
    local session="$1"
    local port="$2"

    screen_stop "$session"

    if [ -n "$port" ]; then
        local pids
        pids=$(lsof -ti "TCP:$port" 2>/dev/null || true)
        if [ -n "$pids" ]; then
            echo "$pids" | xargs kill 2>/dev/null || true
        fi
    fi
}

# ---------------------------------------------------------------------------
# wait_unix_socket — Poll until a Unix socket file exists
#
# Usage:
#   wait_unix_socket SOCKET_PATH [TIMEOUT_SECS]
#
# Arguments:
#   SOCKET_PATH   - Path to the Unix socket file
#   TIMEOUT_SECS  - Timeout in seconds (default: 10)
# ---------------------------------------------------------------------------
wait_unix_socket() {
    local sock="$1"
    local timeout="${2:-10}"
    local iters=$(( timeout * 5 ))  # 0.2s per iteration

    if [ -z "$sock" ]; then
        echo "ERROR: wait_unix_socket: SOCKET_PATH required" >&2
        return 1
    fi

    for i in $(seq 1 "$iters"); do
        if [ -S "$sock" ]; then
            echo "✓ Unix socket ready: $sock"
            return 0
        fi
        sleep 0.2
    done

    echo "ERROR: wait_unix_socket: socket did not appear within ${timeout}s: $sock" >&2
    return 1
}

# ---------------------------------------------------------------------------
# wait_tcp_port — Poll until a TCP port responds with HTTP 200
#
# Usage:
#   wait_tcp_port PORT [TIMEOUT_SECS]
#
# Arguments:
#   PORT          - TCP port to poll on 127.0.0.1
#   TIMEOUT_SECS  - Timeout in seconds (default: 10)
# ---------------------------------------------------------------------------
wait_tcp_port() {
    local port="$1"
    local timeout="${2:-10}"
    local iters=$(( timeout * 5 ))

    if [ -z "$port" ]; then
        echo "ERROR: wait_tcp_port: PORT required" >&2
        return 1
    fi

    for i in $(seq 1 "$iters"); do
        if curl -sf "http://127.0.0.1:$port/" >/dev/null 2>&1; then
            echo "✓ TCP port $port is responding"
            return 0
        fi
        sleep 0.2
    done

    echo "ERROR: wait_tcp_port: port $port did not respond within ${timeout}s" >&2
    return 1
}

# ---------------------------------------------------------------------------
# wait_health — Poll a Hero binary's health endpoint via Unix socket
#
# Usage:
#   wait_health BINARY SOCKET_PATH [TIMEOUT_SECS] [LOG_FILE]
#
# Arguments:
#   BINARY        - Path to the CLI binary (must support --socket SOCK health)
#   SOCKET_PATH   - Unix socket path
#   TIMEOUT_SECS  - Timeout in seconds (default: 60)
#   LOG_FILE      - Log file to tail on failure (optional)
# ---------------------------------------------------------------------------
wait_health() {
    local bin="$1"
    local sock="$2"
    local timeout="${3:-60}"
    local log_file="${4:-}"
    local iters=$(( timeout * 5 ))

    if [ -z "$bin" ] || [ -z "$sock" ]; then
        echo "ERROR: wait_health: BINARY and SOCKET_PATH required" >&2
        return 1
    fi

    for i in $(seq 1 "$iters"); do
        if "$bin" --socket "$sock" health >/dev/null 2>&1; then
            echo "✓ Health check passed: $bin"
            return 0
        fi
        sleep 0.2
    done

    echo "ERROR: wait_health: $bin did not become healthy within ${timeout}s" >&2
    if [ -n "$log_file" ] && [ -f "$log_file" ]; then
        echo "Last log lines:" >&2
        tail -30 "$log_file" >&2
    fi
    return 1
}

# ---------------------------------------------------------------------------
# hero_service_stop — Full stop sequence for a Hero service
#
# Performs: graceful CLI shutdown → wait → force-kill → screen quit → rm sockets
#
# Usage:
#   hero_service_stop SERVER_SOCKET [FORCE] [RELEASE_DIR]
#
# Arguments:
#   SERVER_SOCKET  - Path to the server's Unix socket
#   FORCE          - Set to "1" for immediate kill (skip graceful wait)
#   RELEASE_DIR    - Directory containing hero_proc CLI binary (optional)
# ---------------------------------------------------------------------------
hero_service_stop() {
    local sock="$1"
    local force="${2:-0}"
    local release_dir="${3:-}"

    # Try graceful shutdown via CLI if socket exists
    if [ -S "$sock" ]; then
        local cli=""
        for candidate in \
            "${release_dir}/hero_proc" \
            "${INSTALL_DIR:-$HOME/hero/bin}/hero_proc" \
            "./target/release/hero_proc" \
            "./target/debug/hero_proc"; do
            if [ -x "$candidate" ]; then cli="$candidate"; break; fi
        done

        if [ -n "$cli" ]; then
            if [ "$force" = "1" ]; then
                echo "Requesting force shutdown..."
                "$cli" --socket "$sock" shutdown --force 2>/dev/null || true
            else
                echo "Requesting graceful shutdown..."
                "$cli" --socket "$sock" shutdown 2>/dev/null || true
            fi
        fi

        local wait_iters=60
        [ "$force" = "1" ] && wait_iters=10
        for i in $(seq 1 "$wait_iters"); do
            if ! pgrep -f "hero_proc_server" >/dev/null 2>&1; then break; fi
            sleep 0.5
        done
    fi

    # Kill nc bridge session and process
    nc_bridge_stop "hero_proc_ui_nc" ""

    # Kill UI
    proc_kill "hero_proc_ui"
    screen_stop "hero_proc_ui"

    # Kill server
    if pgrep -f "hero_proc_server" >/dev/null 2>&1; then
        screen_stop "hero_proc_server"
        proc_kill "hero_proc_server"
    else
        screen_stop "hero_proc_server"
    fi

    # Remove stale socket files
    local sock_dir
    sock_dir="$(dirname "$sock")"
    if [ -d "$sock_dir" ]; then
        for s in "$sock_dir"/*.sock; do
            [ -e "$s" ] || continue
            local pid
            pid=$(lsof -t "$s" 2>/dev/null || true)
            if [ -n "$pid" ]; then
                kill "$pid" 2>/dev/null || true
                sleep 0.3
                kill -9 "$pid" 2>/dev/null || true
            fi
            rm -f "$s"
        done
    fi

    echo "✓ Services stopped"
}

# ---------------------------------------------------------------------------
# hero_service_setup — Prepare directories and clean stale state
#
# Creates required directories, removes stale socket files, truncates logs.
#
# Usage:
#   hero_service_setup SERVER_SOCKET UI_SOCKET CONFIG_DIR LOG_DIR
# ---------------------------------------------------------------------------
hero_service_setup() {
    local server_sock="$1"
    local ui_sock="$2"
    local config_dir="$3"
    local log_dir="$4"

    mkdir -p "$(dirname "$server_sock")" "$(dirname "$ui_sock")" "$config_dir" "$log_dir"
    rm -f "$server_sock" "$ui_sock" 2>/dev/null || true

    local server_name
    server_name="$(basename "$server_sock" .sock)"
    local ui_name
    ui_name="$(basename "$ui_sock" .sock)"

    > "$log_dir/${server_name}.log"
    > "$log_dir/${ui_name}.log"

    echo "✓ Directories ready, stale state cleared"
}

# ---------------------------------------------------------------------------
# hero_service_start_server — Start a Hero server binary in a screen session
#
# Starts the server binary, then waits for health check to pass.
#
# Usage:
#   hero_service_start_server SESSION BIN SOCKET LOG_FILE LOG_LEVEL
# ---------------------------------------------------------------------------
hero_service_start_server() {
    local session="$1"
    local bin="$2"
    local sock="$3"
    local log_file="$4"
    local log_level="${5:-info}"

    if [ -z "$session" ] || [ -z "$bin" ] || [ -z "$sock" ] || [ -z "$log_file" ]; then
        echo "ERROR: hero_service_start_server: SESSION BIN SOCKET LOG_FILE required" >&2
        return 1
    fi

    echo "Starting $session..."
    screen_start "$session" \
        "RUST_LOG=\"$log_level\" \"$bin\" --socket \"$sock\" --log-level \"$log_level\" 2>&1 | tee \"$log_file\""

    # Find the CLI binary for health check
    local cli_bin
    local bin_dir
    bin_dir="$(dirname "$bin")"
    local project
    project="$(basename "$(dirname "$bin_dir")")"  # parent dir name as project hint
    # Try common CLI binary names next to the server binary
    for candidate in \
        "${bin_dir}/hero_proc" \
        "${INSTALL_DIR:-$HOME/hero/bin}/hero_proc" \
        "./target/release/hero_proc" \
        "./target/debug/hero_proc"; do
        if [ -x "$candidate" ]; then cli_bin="$candidate"; break; fi
    done

    if [ -z "$cli_bin" ]; then
        echo "WARNING: hero_service_start_server: CLI binary not found, skipping health check" >&2
        sleep 2
        return 0
    fi

    echo "Waiting for $session health (max 60s)..."
    wait_health "$cli_bin" "$sock" 60 "$log_file"
}

# ---------------------------------------------------------------------------
# hero_service_start_ui — Start a Hero UI binary in a screen session
#
# Starts the UI binary bound to a Unix socket, then waits for the socket.
# NOTE: UI binary takes --socket (Unix socket), never --port.
#
# Usage:
#   hero_service_start_ui SESSION BIN SERVER_SOCKET UI_SOCKET LOG_FILE
# ---------------------------------------------------------------------------
hero_service_start_ui() {
    local session="$1"
    local bin="$2"
    local server_sock="$3"
    local ui_sock="$4"
    local log_file="$5"

    if [ -z "$session" ] || [ -z "$bin" ] || [ -z "$server_sock" ] || [ -z "$ui_sock" ] || [ -z "$log_file" ]; then
        echo "ERROR: hero_service_start_ui: SESSION BIN SERVER_SOCKET UI_SOCKET LOG_FILE required" >&2
        return 1
    fi

    echo "Starting $session..."
    screen_start "$session" \
        "\"$bin\" --hero-socket \"$server_sock\" --socket \"$ui_sock\" 2>&1 | tee \"$log_file\""

    echo "Waiting for $session socket (max 10s)..."
    wait_unix_socket "$ui_sock" 10
}
