#!/usr/bin/env bash
# Installs wraithflow + wf-tui from the latest GitHub Release.
#
#   curl -fsSL https://raw.githubusercontent.com/darkstardevx/wraithflow/main/install.sh | sh
#
# Downloads the release archive matching this machine's OS/arch,
# verifies its sha256 against the checksum GitHub Actions published
# alongside it, and installs both binaries to $WRAITHFLOW_INSTALL_DIR
# (default: ~/.local/bin).
set -euo pipefail

REPO="darkstardevx/wraithflow"
INSTALL_DIR="${WRAITHFLOW_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not found on PATH"; }
need curl
need tar
if ! command -v shasum >/dev/null 2>&1 && ! command -v sha256sum >/dev/null 2>&1; then
  die "need either 'shasum' or 'sha256sum' on PATH"
fi

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux) plat="unknown-linux-gnu" ;;
  Darwin) plat="apple-darwin" ;;
  *) die "unsupported OS: $os (only Linux and macOS have published binaries -- see 'cargo install --git' instead)" ;;
esac

case "$arch" in
  x86_64 | amd64) cpu="x86_64" ;;
  arm64 | aarch64) cpu="aarch64" ;;
  *) die "unsupported architecture: $arch" ;;
esac

target="${cpu}-${plat}"
archive="wraithflow-${target}.tar.gz"

say "Detected ${os}/${arch} -> ${target}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

url="https://github.com/${REPO}/releases/latest/download/${archive}"
say "Downloading ${url}"
curl -fsSL "$url" -o "$tmp/$archive" || die "no published release for ${target} yet -- check https://github.com/${REPO}/releases, or use 'cargo install --git https://github.com/${REPO} wraithflow' to build from source"
curl -fsSL "${url}.sha256" -o "$tmp/$archive.sha256"

say "Verifying checksum"
(
  cd "$tmp"
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$archive.sha256"
  else
    sha256sum -c "$archive.sha256"
  fi
) || die "checksum verification failed -- do not run the extracted binaries"

mkdir -p "$INSTALL_DIR"
tar -xzf "$tmp/$archive" -C "$tmp"
install -m 755 "$tmp/wraithflow" "$INSTALL_DIR/wraithflow"
install -m 755 "$tmp/wf-tui" "$INSTALL_DIR/wf-tui"

say ""
say "Installed to ${INSTALL_DIR}/wraithflow and ${INSTALL_DIR}/wf-tui"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say "note: ${INSTALL_DIR} is not on your PATH -- add it to your shell profile" ;;
esac
say "Run 'wraithflow --help' to get started."
