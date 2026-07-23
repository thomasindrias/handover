#!/bin/sh
# One-command build-from-source installer for sesh.
#
#   curl -fsSL https://raw.githubusercontent.com/thomasindrias/sesh/main/install.sh | sh
#
# Or, from inside a sesh checkout: ./install.sh
set -eu

REPO_URL="https://github.com/thomasindrias/sesh.git"

log() {
    printf '%s\n' "$*"
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_cmd() {
    command -v "$1" >/dev/null 2>&1 || fail "$2"
}

require_cmd git "git is required but was not found. Install it with your platform's package manager, then re-run this script."
require_cmd cargo "cargo (Rust) is required but was not found. Install Rust via https://rustup.rs, then re-run this script."

is_sesh_checkout() {
    [ -f Cargo.toml ] && grep -q '^name = "sesh"' Cargo.toml
}

CLEANUP_DIR=""
cleanup() {
    if [ -n "$CLEANUP_DIR" ] && [ -d "$CLEANUP_DIR" ]; then
        rm -rf "$CLEANUP_DIR"
    fi
}
trap cleanup EXIT

if is_sesh_checkout; then
    log "Building sesh from the current checkout..."
else
    CLEANUP_DIR=$(mktemp -d)
    log "Cloning sesh into a temporary directory..."
    git clone --depth 1 "$REPO_URL" "$CLEANUP_DIR/sesh"
    cd "$CLEANUP_DIR/sesh"
fi

log "Installing sesh (this compiles from source, it may take a minute)..."
cargo install --path . --locked --force

cargo_base="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
install_bin_dir="$cargo_base/bin"

case ":$PATH:" in
    *":$install_bin_dir:"*) ;;
    *)
        log ""
        log "Note: $install_bin_dir is not on your PATH. Add this to your shell profile:"
        log "    export PATH=\"$install_bin_dir:\$PATH\""
        ;;
esac

log ""
log "sesh is installed. Next steps:"
log "    sesh setup claude"
log "    sesh setup codex"
