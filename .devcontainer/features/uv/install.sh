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

# sha256 pinned here, not fetched from the release the tarball comes from. Bump with VERSION.
case "$VERSION/$ARCH" in
  0.13.5/x86_64) SHA256=57ef9d9ccb01fb1bd998df995b0dd221b52f7d7841fbea93f2d4463bf61fb7a9 ;;
  0.13.5/aarch64) SHA256=7906c79adb21c81abd559841ba76bb1d9af9d791acac7a041b2972e03ae16538 ;;
  *) echo "uv feature: no pinned sha256 for $VERSION/$ARCH; add it to install.sh" >&2; exit 1 ;;
esac

ASSET="uv-${ARCH}-unknown-linux-musl.tar.gz"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
curl -fsSL -o "$TMP/$ASSET" "https://github.com/astral-sh/uv/releases/download/${VERSION}/${ASSET}"
echo "$SHA256  $ASSET" | (cd "$TMP" && sha256sum -c -)
tar -xzf "$TMP/$ASSET" -C "$TMP" --strip-components=1
install -m 0755 "$TMP/uv" "$TMP/uvx" /usr/local/bin/
uv --version
