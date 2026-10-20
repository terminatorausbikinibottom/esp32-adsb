#!/usr/bin/env bash
set -euo pipefail

VERSION="${VERSION:-stable}"
case "$(uname -m)" in
  x86_64) ARCH=x86_64 ;;
  aarch64 | arm64) ARCH=arm64 ;;
  *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;;
esac

# Neovim 0.11+ also needs ripgrep/fd/tree-sitter build deps for LazyVim-style configs.
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends curl ca-certificates tar gzip unzip build-essential fd-find ripgrep
rm -rf /var/lib/apt/lists/*

rm -rf /opt/nvim
mkdir -p /opt/nvim
curl -fsSL "https://github.com/neovim/neovim/releases/download/${VERSION}/nvim-linux-${ARCH}.tar.gz" \
  | tar -xz -C /opt/nvim --strip-components=1
ln -sf /opt/nvim/bin/nvim /usr/local/bin/nvim
ln -sf "$(command -v fdfind)" /usr/local/bin/fd
nvim --version | head -1
