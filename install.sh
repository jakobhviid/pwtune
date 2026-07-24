#!/bin/sh
# pwtune installer — no Homebrew, no compiler, no root. Downloads the prebuilt
# binary for your architecture into a bin dir on your PATH. Linux only.
#
#   curl -fsSL https://raw.githubusercontent.com/jakobhviid/pwtune/main/install.sh | sh
#
# Override the target dir with PWTUNE_BIN_DIR.
set -eu

REPO="jakobhviid/pwtune"
NAME="pwtune"
BIN_DIR="${PWTUNE_BIN_DIR:-$HOME/.local/bin}"

os="$(uname -s)"
arch="$(uname -m)"

if [ "$os" != "Linux" ]; then
    echo "pwtune is Linux-only (it configures PipeWire); got: $os" >&2
    exit 1
fi

case "$arch" in
    x86_64 | amd64)   target_arch="x86_64" ;;
    aarch64 | arm64)  target_arch="aarch64" ;;
    *) echo "unsupported architecture: $arch" >&2; exit 1 ;;
esac

asset="${NAME}-${target_arch}-unknown-linux-musl.tar.gz"
url="https://github.com/${REPO}/releases/latest/download/${asset}"

echo "Installing ${NAME} (${target_arch}) → ${BIN_DIR}"
mkdir -p "$BIN_DIR"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if ! curl -fsSL "$url" | tar xz -C "$tmp"; then
    echo "download/extract failed: $url" >&2
    exit 1
fi

installed=""
for f in "$tmp"/*; do
    [ -f "$f" ] || continue
    name="$(basename "$f")"
    install -m 0755 "$f" "$BIN_DIR/$name"
    installed="${installed} ${name}"
done

echo "Installed:${installed}"

case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "note: ${BIN_DIR} is not on your PATH — add it, e.g.:"
       echo "      export PATH=\"${BIN_DIR}:\$PATH\"" ;;
esac

echo "Done."
