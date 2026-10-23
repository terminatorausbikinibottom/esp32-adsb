#!/usr/bin/env bash
set -euo pipefail

VERSION="${VERSION:-}"
[[ -n "$VERSION" ]] || { echo "uv feature: 'version' option is required (pinned, no 'latest')" >&2; exit 1; }
case "$(uname -m)" in
  x86_64) ARCH=x86_64 ;;
  aarch64 | arm64) ARCH=aarch64 ;;
  *) echo "unsupported arch: $(uname -m)" >&2; exit 1 ;;
esac

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends curl ca-certificates tar gzip
rm -rf /var/lib/apt/lists/*

ASSET="uv-${ARCH}-unknown-linux-musl.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
curl -fsSL -o "$TMP/$ASSET" "https://github.com/astral-sh/uv/releases/download/${VERSION}/${ASSET}"
curl -fsSL -o "$TMP/$ASSET.sha256" "https://github.com/astral-sh/uv/releases/download/${VERSION}/${ASSET}.sha256"
(cd "$TMP" && sha256sum -c "$ASSET.sha256")
tar -xzf "$TMP/$ASSET" -C "$TMP" --strip-components=1
install -m 0755 "$TMP/uv" "$TMP/uvx" /usr/local/bin/
uv --version
