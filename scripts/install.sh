#!/bin/sh
# Install cadence from a GitHub release (CAD-311).
#
#   curl -fsSL https://github.com/favcrm/cadence/releases/latest/download/install.sh | sh
#   sh install.sh --version v0.1.0 --prefix /opt/cadence
#
# Everything runs from `main`, called on the last line with a closing
# `--end-of-script` marker that `main` requires: a download cut short
# fails to parse, stops before that call, or calls `main` without the
# marker — so it installs nothing and never reports success.
#
# What it does, in order:
#   1. picks the asset for this machine: x86_64-linux, aarch64-linux or
#      aarch64-macos;
#   2. downloads cadence-<tag>-<target>.tar.gz and its .sha256 and refuses
#      to go on unless the tarball hashes to the published value;
#   3. checks the binary inside against the tarball's own cadence.sha256;
#   4. installs it to <prefix>/releases/<tag>/ (cadence, cadence.sha256,
#      manifest.json) — the same release layout `cadence upgrade` uses,
#      where <prefix> defaults to ${XDG_DATA_HOME:-~/.local/share}/cadence;
#   5. points ~/.local/bin/cadence at <prefix>/releases/<tag>/cadence
#      (a new link renamed over the old one) and runs `cadence --version`.
#
# Rerunning is safe: an installed release with the same bytes is kept and
# only relinked. A release dir holding different bytes, or a
# ~/.local/bin/cadence that is not a symlink, is never overwritten.
#
# The checksums come from the same place as the tarball, so they catch
# corruption and truncation, not a forged release; authenticity is the
# build-provenance attestation (`gh attestation verify`, see
# docs/INSTALL-AGENT.md).
#
# POSIX sh; needs curl (or wget for https), tar, and sha256sum, shasum or
# openssl. Environment overrides (for mirrors and tests):
#   CADENCE_INSTALL_BASE_URL  assets live at <base>/<tag>/<asset>
#   CADENCE_INSTALL_API_URL   `latest` is read from <api>/releases/latest

set -eu

say() { printf 'cadence-install: %s\n' "$*"; }
die() {
    printf 'cadence-install: error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: install.sh [--version <tag>|latest] [--prefix <dir>] [--base-url <url>]

  --version   release tag to install, e.g. v0.1.0 (default: latest)
  --prefix    install root; releases go to <prefix>/releases/<tag>
              (default: ${XDG_DATA_HOME:-$HOME/.local/share}/cadence)
  --base-url  download assets from <url>/<tag>/<asset> instead of
              GitHub Releases (https:// or file://)

The binary is linked at $HOME/.local/bin/cadence.
EOF
}

# --- tools -----------------------------------------------------------------

have() { command -v "$1" >/dev/null 2>&1; }

# fetch <url> <file>: file:// only through curl; https through curl or wget.
fetch() {
    if have curl; then
        curl --proto '=https,file' --tlsv1.2 -fsSL --retry 3 -o "$2" "$1" \
            || die "download failed: $1"
    elif have wget; then
        case "$1" in
            https://*) wget -q -O "$2" "$1" || die "download failed: $1" ;;
            *) die "wget cannot fetch $1 — install curl" ;;
        esac
    else
        die "need curl or wget to download releases"
    fi
}

sha256_of() {
    if have sha256sum; then
        sha256sum "$1" | cut -d' ' -f1
    elif have shasum; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif have openssl; then
        openssl dgst -sha256 -r "$1" | cut -d' ' -f1
    else
        die "need sha256sum, shasum or openssl to verify the download"
    fi
}

# The first field of a `sha256sum`-style line, lowercased, if it is 64 hex.
recorded_sha256() {
    h=$(sed -n '1s/[[:space:]].*//p' "$1" | tr 'A-F' 'a-f')
    case "$h" in
        *[!0-9a-f]* | '') die "$1 does not hold a sha256" ;;
    esac
    [ ${#h} -eq 64 ] || die "$1 does not hold a sha256"
    printf '%s\n' "$h"
}

cleanup() {
    rm -rf "$TMP"
    if [ -n "$STAGE" ]; then rm -rf "$STAGE"; fi
}

main() {
    REPO=favcrm/cadence
    DEFAULT_BASE_URL=https://github.com/$REPO/releases/download
    BASE_URL=${CADENCE_INSTALL_BASE_URL:-$DEFAULT_BASE_URL}
    API_URL=${CADENCE_INSTALL_API_URL:-https://api.github.com/repos/$REPO}
    VERSION=latest
    PREFIX=
    COMPLETE=

    while [ $# -gt 0 ]; do
        case "$1" in
            --version | --prefix | --base-url)
                [ $# -ge 2 ] || die "$1 needs a value"
                case "$1" in
                    --version) VERSION=$2 ;;
                    --prefix) PREFIX=$2 ;;
                    --base-url) BASE_URL=$2 ;;
                esac
                shift 2
                ;;
            --version=*) VERSION=${1#*=}; shift ;;
            --prefix=*) PREFIX=${1#*=}; shift ;;
            --base-url=*) BASE_URL=${1#*=}; shift ;;
            -h | --help) usage; exit 0 ;;
            --end-of-script) COMPLETE=1; shift ;;
            *) usage >&2; die "unknown argument: $1" ;;
        esac
    done

    [ -n "$COMPLETE" ] || die "install.sh was cut short in transit — download it again"
    [ -n "${HOME:-}" ] || die "HOME is not set"
    if [ -z "$PREFIX" ]; then
        PREFIX=${XDG_DATA_HOME:-$HOME/.local/share}/cadence
    fi
    case "$PREFIX" in
        /*) ;;
        *) PREFIX=$(pwd)/$PREFIX ;;
    esac
    BIN_DIR=$HOME/.local/bin
    LINK=$BIN_DIR/cadence

    have tar || die "need tar to unpack the release"

    # --- platform --------------------------------------------------------------

    os=$(uname -s)
    arch=$(uname -m)
    case "$arch" in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) die "unsupported CPU: $arch (releases exist for x86_64 and aarch64)" ;;
    esac
    case "$os" in
        Linux) TARGET=$arch-linux ;;
        Darwin)
            # A shell under Rosetta reports x86_64 on Apple silicon.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                arch=aarch64
            fi
            [ "$arch" = aarch64 ] || die "unsupported: Intel macOS (releases exist for Apple silicon)"
            TARGET=aarch64-macos
            ;;
        *) die "unsupported OS: $os (releases exist for Linux and macOS)" ;;
    esac

    # --- scratch space ---------------------------------------------------------

    TMP=$(mktemp -d "${TMPDIR:-/tmp}/cadence-install.XXXXXX") || die "mktemp failed"
    STAGE=
    trap cleanup EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM

    # --- version ---------------------------------------------------------------

    if [ "$VERSION" = latest ]; then
        fetch "$API_URL/releases/latest" "$TMP/latest.json"
        VERSION=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$TMP/latest.json" | head -n 1)
        [ -n "$VERSION" ] || die "could not read the latest release tag from $API_URL/releases/latest"
        if [ "$API_URL" != "https://api.github.com/repos/$REPO" ]; then
            say "latest: read from $API_URL/releases/latest (not the default)"
        fi
    fi
    case "$VERSION" in
        v*) TAG=$VERSION ;;
        *) TAG=v$VERSION ;;
    esac
    # The tag becomes a directory name and part of a URL: keep it plain.
    case "$TAG" in
        v | *[!0-9A-Za-z.+_-]* | *..*) die "not a release tag: $VERSION" ;;
    esac

    ASSET=cadence-$TAG-$TARGET.tar.gz
    URL=$BASE_URL/$TAG/$ASSET
    DEST=$PREFIX/releases/$TAG

    say "installing cadence $TAG ($TARGET) into $DEST"
    if [ "$BASE_URL" != "$DEFAULT_BASE_URL" ]; then
        say "source: $URL (not the default $DEFAULT_BASE_URL)"
    fi

    # --- download and verify ---------------------------------------------------

    fetch "$URL.sha256" "$TMP/$ASSET.sha256"
    expected=$(recorded_sha256 "$TMP/$ASSET.sha256")
    fetch "$URL" "$TMP/$ASSET"
    actual=$(sha256_of "$TMP/$ASSET")
    if [ "$actual" != "$expected" ]; then
        die "checksum mismatch for $ASSET: published $expected, downloaded file hashes to $actual — refusing to install"
    fi
    say "checksum ok: $actual"

    mkdir "$TMP/x"
    tar -xzf "$TMP/$ASSET" -C "$TMP/x" || die "could not unpack $ASSET"
    for f in cadence cadence.sha256 manifest.json; do
        [ -f "$TMP/x/$f" ] || die "$ASSET is missing $f — refusing to install"
    done
    digest=$(recorded_sha256 "$TMP/x/cadence.sha256")
    [ "$(sha256_of "$TMP/x/cadence")" = "$digest" ] \
        || die "the binary in $ASSET does not match its cadence.sha256 — refusing to install"

    # --- install ---------------------------------------------------------------

    mkdir -p "$PREFIX/releases"
    if [ -e "$DEST" ] || [ -L "$DEST" ]; then
        if [ ! -f "$DEST/cadence" ] || [ "$(sha256_of "$DEST/cadence")" != "$digest" ]; then
            die "$DEST exists but does not hold this release's binary — move it aside and rerun"
        fi
        say "already installed: $DEST/cadence"
    else
        # Stage beside the destination, then rename: a present release dir
        # is always complete.
        STAGE=$(mktemp -d "$PREFIX/releases/.install-$TAG.XXXXXX") || die "mktemp in $PREFIX/releases failed"
        for f in manifest.json cadence.sha256 cadence; do
            cp "$TMP/x/$f" "$STAGE/$f"
        done
        chmod 0644 "$STAGE/manifest.json" "$STAGE/cadence.sha256"
        chmod 0755 "$STAGE/cadence" "$STAGE"
        mv "$STAGE" "$DEST" || die "could not move the release into $DEST"
        STAGE=
        [ "$(sha256_of "$DEST/cadence")" = "$digest" ] \
            || die "installed copy $DEST/cadence does not hash to $digest — the link was not moved"
        say "installed $DEST/cadence"
    fi

    # --- link ------------------------------------------------------------------

    mkdir -p "$BIN_DIR"
    if [ -L "$LINK" ]; then
        [ ! -d "$LINK" ] || die "$LINK is a symlink to a directory — refusing to replace it"
    elif [ -e "$LINK" ]; then
        die "$LINK exists and is not a symlink — refusing to replace it; move it aside and rerun"
    fi
    if [ "$(readlink "$LINK" 2>/dev/null || true)" = "$DEST/cadence" ]; then
        say "link unchanged: $LINK -> $DEST/cadence"
    else
        tmp_link=$BIN_DIR/.cadence.install-$$
        rm -f "$tmp_link"
        ln -s "$DEST/cadence" "$tmp_link"
        mv -f "$tmp_link" "$LINK" || {
            rm -f "$tmp_link"
            die "could not link $LINK"
        }
        say "linked $LINK -> $DEST/cadence"
    fi

    # --- check -----------------------------------------------------------------

    version_out=$("$LINK" --version) || die "$LINK --version failed"
    case "$version_out" in
        "cadence ${TAG#v}+"*) ;;
        *) die "$LINK --version reports '$version_out', expected cadence ${TAG#v}+<commit>" ;;
    esac
    say "ok: $version_out"

    case ":${PATH:-}:" in
        *":$BIN_DIR:"*) ;;
        *) say "note: $BIN_DIR is not on PATH — add it, e.g. export PATH=\"$BIN_DIR:\$PATH\"" ;;
    esac
}

# The marker must stay last on this line (see the header).
main "$@" --end-of-script
