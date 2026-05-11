#!/bin/sh
# isomage installer — fetches the latest release binary for your platform.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/JackDanger/isomage/main/install.sh | sh
#
# Environment variables:
#   VERSION   release tag to install (default: latest, e.g. "v0.3.0")
#   BIN_DIR   install directory     (default: /usr/local/bin)

set -eu

REPO="JackDanger/isomage"
BIN_DIR="${BIN_DIR:-/usr/local/bin}"
VERSION="${VERSION:-latest}"

err()  { printf 'install.sh: %s\n' "$*" >&2; exit 1; }
info() { printf '%s\n' "$*"; }

# --- detect platform ---------------------------------------------------------
os="$(uname -s)"
case "$os" in
  Darwin) os=macos ;;
  Linux)  os=linux ;;
  *) err "unsupported OS: $os (need macOS or Linux)" ;;
esac

arch="$(uname -m)"
case "$arch" in
  arm64|aarch64) arch=arm64 ;;
  x86_64|amd64)  arch=x86_64 ;;
  *) err "unsupported arch: $arch (need arm64 or x86_64)" ;;
esac

asset="isomage-${os}-${arch}"
tarball="${asset}.tar.gz"

if [ "$VERSION" = latest ]; then
  base="https://github.com/${REPO}/releases/latest/download"
else
  base="https://github.com/${REPO}/releases/download/${VERSION}"
fi

# --- pick a downloader -------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  err "need curl or wget"
fi

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t isomage)"
trap 'rm -rf "$tmp"' EXIT INT TERM

info "Downloading $tarball ..."
fetch "$base/$tarball" "$tmp/$tarball" || err "download failed: $base/$tarball"

# --- verify checksum (best-effort) ------------------------------------------
if fetch "$base/checksums.txt" "$tmp/checksums.txt" 2>/dev/null; then
  expected="$(awk -v f="$tarball" '$2==f || $2=="*"f {print $1; exit}' "$tmp/checksums.txt")"
  if [ -n "$expected" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      actual="$(sha256sum "$tmp/$tarball" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
      actual="$(shasum -a 256 "$tmp/$tarball" | awk '{print $1}')"
    else
      actual=""
    fi
    if [ -n "$actual" ] && [ "$actual" != "$expected" ]; then
      err "checksum mismatch: expected $expected, got $actual"
    fi
  fi
fi

# --- extract -----------------------------------------------------------------
tar -xzf "$tmp/$tarball" -C "$tmp" || err "extraction failed"
[ -f "$tmp/$asset" ] || err "tarball did not contain expected binary: $asset"

# --- install (escalate only if needed) --------------------------------------
SUDO=""
if [ -d "$BIN_DIR" ]; then
  [ -w "$BIN_DIR" ] || SUDO=sudo
else
  parent="$(dirname "$BIN_DIR")"
  [ -w "$parent" ] || SUDO=sudo
fi
if [ -n "$SUDO" ] && [ "$(id -u)" = 0 ]; then
  SUDO=""
fi
if [ -n "$SUDO" ]; then
  command -v sudo >/dev/null 2>&1 || err "$BIN_DIR not writable and sudo unavailable; set BIN_DIR=<writable dir>"
  info "Installing to $BIN_DIR (will prompt for sudo) ..."
fi

$SUDO mkdir -p "$BIN_DIR"
$SUDO install -m 0755 "$tmp/$asset" "$BIN_DIR/isomage"

info "Installed: $BIN_DIR/isomage"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) info "Note: $BIN_DIR is not on your PATH." ;;
esac
info "Run 'isomage --help' to get started."
