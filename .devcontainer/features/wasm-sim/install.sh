#!/usr/bin/env bash
# Installs the browser-simulator toolchain for the container's remote user.
# Expects the devcontainers rust feature (CARGO_HOME / RUSTUP_HOME in /usr/local).
#
# wasm-bindgen strategy: trunk reads the `wasm-bindgen` version from Cargo.lock and uses a
# wasm-bindgen-cli on PATH only if the version matches exactly; otherwise it downloads the
# matching one (and wasm-opt) into its own cache. So pre-installing is an offline convenience,
# not a requirement. Keep wasmBindgenVersion equal to the sim's locked `wasm-bindgen` version.
set -euo pipefail

TRUNK_VERSION="${TRUNKVERSION:-}"
WASM_BINDGEN_VERSION="${WASMBINDGENVERSION:-}"
USERNAME="${_REMOTE_USER:-vscode}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found: add ghcr.io/devcontainers/features/rust before wasm-sim" >&2
  exit 1
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends curl ca-certificates
rm -rf /var/lib/apt/lists/*

# Run as the remote user so files in the shared rustup/cargo homes keep the right owner.
as_user() {
  sudo -u "$USERNAME" env \
    HOME="$(getent passwd "$USERNAME" | cut -d: -f6)" \
    CARGO_HOME="$CARGO_HOME" RUSTUP_HOME="$RUSTUP_HOME" PATH="$PATH" \
    ${GITHUB_TOKEN:+GITHUB_TOKEN="$GITHUB_TOKEN"} \
    bash -euo pipefail -c "$1"
}

as_user "
  rustup target add wasm32-unknown-unknown --toolchain stable
  command -v cargo-binstall >/dev/null ||
    curl -fsSL https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
  cargo binstall -y --locked trunk${TRUNK_VERSION:+@$TRUNK_VERSION}
  ${WASM_BINDGEN_VERSION:+cargo binstall -y --locked wasm-bindgen-cli@$WASM_BINDGEN_VERSION}
"

echo "wasm-sim tooling installed for $USERNAME (trunk ${TRUNK_VERSION:-latest}, wasm-bindgen-cli ${WASM_BINDGEN_VERSION:-none})"
