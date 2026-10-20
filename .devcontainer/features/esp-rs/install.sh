#!/usr/bin/env bash
# Installs the esp-rs toolchain for the container's remote user.
# Expects the devcontainers rust feature (CARGO_HOME / RUSTUP_HOME in /usr/local).
set -euo pipefail

TARGETS="${TARGETS:-all}"
TOOLCHAIN_VERSION="${TOOLCHAINVERSION:-}"
USERNAME="${_REMOTE_USER:-vscode}"
EXPORT_FILE=/usr/local/share/esp-rs/export-esp.sh

if ! command -v cargo >/dev/null 2>&1; then
  echo "cargo not found: add ghcr.io/devcontainers/features/rust before esp-rs" >&2
  exit 1
fi

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
# libudev/pkg-config: espflash serial support; python3/venv/pip: esptool & misc scripts.
apt-get install -y -qq --no-install-recommends \
  libudev-dev pkg-config curl ca-certificates python3 python3-venv python3-pip
rm -rf /var/lib/apt/lists/*

# Run as the remote user so files in the shared rustup/cargo homes keep the right owner.
as_user() {
  sudo -u "$USERNAME" env \
    HOME="$(getent passwd "$USERNAME" | cut -d: -f6)" \
    CARGO_HOME="$CARGO_HOME" RUSTUP_HOME="$RUSTUP_HOME" PATH="$PATH" \
    ${GITHUB_TOKEN:+GITHUB_TOKEN="$GITHUB_TOKEN"} \
    bash -euo pipefail -c "$1"
}

install -d -o "$USERNAME" "$(dirname "$EXPORT_FILE")"

as_user "
  curl -fsSL https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash
  cargo binstall -y --locked espup espflash cargo-espflash esp-generate
  espup install --targets '$TARGETS' ${TOOLCHAIN_VERSION:+--toolchain-version '$TOOLCHAIN_VERSION'} \
    --export-file '$EXPORT_FILE'
"

# Source the espup environment (LIBCLANG_PATH, Xtensa GCC on PATH) for every shell.
# Use system-wide rc files so user dotfiles (chezmoi) don't need to know about it.
SNIPPET="[ -f $EXPORT_FILE ] && . $EXPORT_FILE"
for rc in /etc/zsh/zshenv /etc/bash.bashrc /etc/profile.d/esp-rs.sh; do
  mkdir -p "$(dirname "$rc")"
  grep -qxF "$SNIPPET" "$rc" 2>/dev/null || echo "$SNIPPET" >> "$rc"
done

# Serial access when a device is passed through (Linux hosts).
usermod -aG dialout "$USERNAME" || true

echo "esp-rs toolchain installed for $USERNAME (targets: $TARGETS)"
