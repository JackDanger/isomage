#!/usr/bin/env sh
# isomage installer — downloads the latest pre-built binary for your platform
# from https://github.com/JackDanger/isomage/releases and installs it.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/JackDanger/isomage/main/install.sh | sh
#
# Environment overrides:
#   ISOMAGE_VERSION   pin to a specific tag (e.g. v0.4.0); default: latest
#   ISOMAGE_PREFIX    install dir; default: /usr/local/bin if writable,
#                     escalating to sudo if needed, else $HOME/.local/bin
#   ISOMAGE_REPO      override owner/name; default: JackDanger/isomage

set -eu

REPO="${ISOMAGE_REPO:-JackDanger/isomage}"
VERSION="${ISOMAGE_VERSION:-}"

# ---- helpers ---------------------------------------------------------------

err()  { printf 'install.sh: error: %s\n' "$*" >&2; exit 1; }
info() { printf '==> %s\n' "$*"; }

# Pick a downloader at call time. Curl is preferred (better progress + retry),
# but plenty of minimal Debian/Ubuntu images ship only wget.
download() {
    url="$1"; dest="$2"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$dest" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$dest" "$url"
    else
        err "neither curl nor wget found; install one and retry"
    fi
}

# Resolve "latest" to a concrete tag via the /releases/latest redirect, so log
# lines and the eventual asset URL agree on a version. No JSON parser, no
# GitHub token needed.
resolve_latest() {
    if command -v curl >/dev/null 2>&1; then
        location=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
            "https://github.com/${REPO}/releases/latest")
        printf '%s\n' "${location##*/}"
    else
        # wget can't print the final Location portably; fall back to the API.
        wget -q -O - "https://api.github.com/repos/${REPO}/releases/latest" \
            | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1
    fi
}

# Run a privileged command. Sudoes only if we're not already root.
as_root() {
    if [ "$(id -u 2>/dev/null || echo 0)" -eq 0 ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        err "this step requires root; install sudo or run as root: $*"
    fi
}

# ---- detect platform -------------------------------------------------------

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
    Linux)  os_slug="linux" ;;
    Darwin) os_slug="macos" ;;
    *)      err "unsupported OS: $OS (only Linux and macOS have pre-built binaries)" ;;
esac

case "$ARCH" in
    x86_64|amd64)   arch_slug="x86_64" ;;
    aarch64|arm64)  arch_slug="arm64" ;;
    *)              err "unsupported architecture: $ARCH" ;;
esac

# ---- resolve version & asset -----------------------------------------------

if [ -z "$VERSION" ]; then
    info "resolving latest release"
    VERSION=$(resolve_latest)
    [ -n "$VERSION" ] || err "could not determine latest release tag"
fi
# Accept "0.4.0" as well as "v0.4.0".
case "$VERSION" in
    v*) ;;
    *)  VERSION="v${VERSION}" ;;
esac

ASSET="isomage-${os_slug}-${arch_slug}"
TARBALL="${ASSET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${TARBALL}"
SUMS_URL="https://github.com/${REPO}/releases/download/${VERSION}/checksums.txt"

info "selected: $TARBALL ($VERSION)"

# ---- pick install dir ------------------------------------------------------

# Precedence: ISOMAGE_PREFIX > /usr/local/bin (direct write) > /usr/local/bin
# via sudo > $HOME/.local/bin. Falling back to ~/.local/bin keeps the one-liner
# working in rootless environments — containers, CI runners, devcontainers —
# where /usr/local/bin isn't writable and sudo is missing.
USE_SUDO=0
if [ -n "${ISOMAGE_PREFIX:-}" ]; then
    PREFIX="$ISOMAGE_PREFIX"
    # User picked the dir explicitly; if it exists but isn't writable, escalate
    # rather than failing later in the install step.
    if [ -d "$PREFIX" ] && [ ! -w "$PREFIX" ] && command -v sudo >/dev/null 2>&1; then
        USE_SUDO=1
    fi
elif [ -w /usr/local/bin ] 2>/dev/null; then
    PREFIX=/usr/local/bin
elif command -v sudo >/dev/null 2>&1 && [ -d /usr/local/bin ]; then
    PREFIX=/usr/local/bin
    USE_SUDO=1
else
    PREFIX="$HOME/.local/bin"
fi
mkdir -p "$PREFIX" 2>/dev/null || as_root mkdir -p "$PREFIX"

# ---- download, verify, install ---------------------------------------------

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t isomage)
trap 'rm -rf "$tmp"' EXIT

info "downloading $URL"
download "$URL" "$tmp/$TARBALL"

# Verify against the aggregate checksums.txt the release workflow publishes.
# Tolerate its absence so a release that omits it still installs.
if download "$SUMS_URL" "$tmp/checksums.txt" 2>/dev/null; then
    # Filter to our asset's line so `-c` only checks the file we have. Handle
    # both text-mode ("<hex>  name") and binary-mode ("<hex>  *name") formats.
    awk -v f="$TARBALL" '$2==f || $2=="*"f {print; exit}' \
        "$tmp/checksums.txt" > "$tmp/$TARBALL.sha256"
    if [ -s "$tmp/$TARBALL.sha256" ]; then
        if command -v sha256sum >/dev/null 2>&1; then
            ( cd "$tmp" && sha256sum -c "$TARBALL.sha256" ) >/dev/null \
                || err "sha256 verification failed for $TARBALL"
            info "sha256 verified"
        elif command -v shasum >/dev/null 2>&1; then
            ( cd "$tmp" && shasum -a 256 -c "$TARBALL.sha256" ) >/dev/null \
                || err "sha256 verification failed for $TARBALL"
            info "sha256 verified"
        fi
    fi
fi

info "extracting"
( cd "$tmp" && tar -xzf "$TARBALL" )
[ -f "$tmp/$ASSET" ] || err "tarball did not contain expected binary: $ASSET"
chmod +x "$tmp/$ASSET"

info "installing to $PREFIX/isomage"
# Use an explicit if/elif chain rather than `install || cp && chmod`:
# that parses as `(install || cp) && chmod`, and POSIX `set -e` is suppressed
# for non-final commands of an AND-OR list, so a `cp` failure would silently
# leave the user without a binary.
if [ "$USE_SUDO" = "1" ]; then
    as_root install -m 755 "$tmp/$ASSET" "$PREFIX/isomage" \
        || err "failed to install isomage to $PREFIX (sudo install)"
elif install -m 755 "$tmp/$ASSET" "$PREFIX/isomage" 2>/dev/null; then
    :
elif cp "$tmp/$ASSET" "$PREFIX/isomage" 2>/dev/null && chmod 755 "$PREFIX/isomage"; then
    :
else
    err "failed to install isomage to $PREFIX (no write access, no install(1), and cp failed)"
fi

# ---- post-install summary --------------------------------------------------

case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *) printf '\nNote: %s is not on your PATH. Add this to your shell rc:\n  export PATH="%s:$PATH"\n' \
              "$PREFIX" "$PREFIX" ;;
esac

isomage_version=$("$PREFIX/isomage" --version 2>/dev/null || printf 'isomage')

printf '\n'
info "$isomage_version installed at $PREFIX/isomage"
printf '\nTry:\n  isomage --help\n'
