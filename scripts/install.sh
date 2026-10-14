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
  --client-only  install a client-only cadence-client from the public
              GitHub release into its own prefix (default
              ${XDG_DATA_HOME:-$HOME/.local/share}/cadence-client), linked at
              $HOME/.local/bin/cadence-client; no --base-url or CADENCE_INSTALL_*

The binary is linked at $HOME/.local/bin/cadence.
EOF
}

# --- tools -----------------------------------------------------------------


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
    if [ -n "$LOCK" ]; then rmdir "$LOCK" 2>/dev/null || true; fi
}

# --- client-only (CAD-1425) -----------------------------------------------

# release_key <tag>: "MAJOR MINOR PATCH" for vMAJOR.MINOR.PATCH with plain
# digits; fails for anything else (the same rule as `cadence update`).
release_key() {
    case "$1" in
        v*.*.*) ;;
        *) return 1 ;;
    esac
    rest=${1#v}
    maj=${rest%%.*}
    rest=${rest#*.}
    min=${rest%%.*}
    pat=${rest#*.}
    case "$pat" in *.*) return 1 ;; esac
    for part in "$maj" "$min" "$pat"; do
        case "$part" in
            '' | *[!0-9]*) return 1 ;;
        esac
        [ "${#part}" -le 9 ] || return 1
    done
    printf '%s %s %s\n' "$maj" "$min" "$pat"
}

# release_newer <tag> <than>: succeeds only when both are plain release tags
# and <tag> is strictly newer.
release_newer() {
    a=$(release_key "$1") || return 1
    b=$(release_key "$2") || return 1
    set -- $a
    a1=$1 a2=$2 a3=$3
    set -- $b
    b1=$1 b2=$2 b3=$3
    if [ "$a1" -ne "$b1" ]; then [ "$a1" -gt "$b1" ]; return; fi
    if [ "$a2" -ne "$b2" ]; then [ "$a2" -gt "$b2" ]; return; fi
    [ "$a3" -gt "$b3" ]
}

# client_receipt <target>: the exact client-install.json for this prefix.
client_receipt() {
    printf '{\n  "schema": 1,\n  "kind": "client-only",\n  "prefix": "%s",\n  "link": "%s",\n  "target": "%s"\n}\n' \
        "$PREFIX" "$CLIENT_LINK" "$1"
}

# client_check_prefix: refuse any prefix or link that is not a fresh empty
# prefix or this installation's own client-only prefix. Runs before any
# network access or write. Sets CLIENT_CURRENT to the installed tag, if any.
client_check_prefix() {
    CLIENT_CURRENT=
    for p in "$PREFIX" "$CLIENT_LINK"; do
        case "$p" in
            *\"* | *\\*) die "client-only paths cannot hold quotes or backslashes: $p" ;;
        esac
        [ "$(printf '%s' "$p" | LC_ALL=C tr -d '\000-\037\177')" = "$p" ] \
            || die "client-only paths cannot hold control characters: $p"
    done
    if [ -L "$PREFIX" ]; then
        die "$PREFIX is a symlink — refusing to use it as a client-only prefix"
    fi
    if [ -e "$PREFIX" ] && [ ! -d "$PREFIX" ]; then
        die "$PREFIX exists and is not a directory"
    fi
    if [ -e "$PREFIX/releases" ] || [ -L "$PREFIX/releases" ]; then
        [ -d "$PREFIX/releases" ] && [ ! -L "$PREFIX/releases" ] \
            || die "$PREFIX/releases is not a plain directory — refusing"
    fi
    receipt=$PREFIX/client-install.json
    if [ -e "$receipt" ] || [ -L "$receipt" ]; then
        [ -f "$receipt" ] && [ ! -L "$receipt" ] \
            || die "$receipt is not a regular file — refusing"
        [ "$(cat "$receipt"; printf x)" = "$(client_receipt "$CLIENT_TARGET"; printf x)" ] \
            || die "$receipt does not match this client-only installation (malformed, or another prefix, link or platform) — refusing"
        if [ ! -e "$CLIENT_LINK" ] && [ ! -L "$CLIENT_LINK" ]; then
            die "$receipt exists but $CLIENT_LINK is missing — refusing to relink; repair or remove the client-only installation by hand"
        fi
    elif [ -d "$PREFIX" ] && [ -n "$(ls -A "$PREFIX")" ]; then
        die "$PREFIX holds files but no client-install.json — refusing to adopt it (an ordinary cadence installation is never reused); choose an empty prefix"
    fi
    if [ -e "$CLIENT_LINK" ] || [ -L "$CLIENT_LINK" ]; then
        if [ ! -e "$receipt" ] && [ ! -L "$receipt" ]; then
            die "$CLIENT_LINK exists and no client-only installation owns it — refusing to replace it"
        fi
        [ -L "$CLIENT_LINK" ] && [ ! -d "$CLIENT_LINK" ] \
            || die "$CLIENT_LINK is not a symlink to a client-only release binary — refusing to replace it"
        linked=$(readlink "$CLIENT_LINK" 2>/dev/null || true)
        case "$linked" in
            "$PREFIX/releases/"*/cadence) ;;
            *) die "$CLIENT_LINK does not point into $PREFIX/releases — refusing to replace it" ;;
        esac
        CLIENT_CURRENT=${linked#"$PREFIX/releases/"}
        CLIENT_CURRENT=${CLIENT_CURRENT%/cadence}
        case "$CLIENT_CURRENT" in
            */*) die "$CLIENT_LINK points at a nested path — refusing to replace it" ;;
        esac
        release_key "$CLIENT_CURRENT" >/dev/null \
            || die "$CLIENT_LINK points at a release that cannot be ordered — refusing to replace it"
        [ -f "$linked" ] || die "$CLIENT_LINK points at a missing release binary — refusing to replace it"
    fi
    CLIENT_SEEN=$(client_seen_state)
}

# client_seen_state: the client link and receipt as the checks last saw them;
# compared again under the commit lock, so a concurrent change refuses.
client_seen_state() {
    printf '%s|%s\n' "$(readlink "$CLIENT_LINK" 2>/dev/null || echo none)" \
        "$(cat "$PREFIX/client-install.json" 2>/dev/null || echo none)"
}

# client_lock: the commit lock shared with `cadence update`, a
# directory beside the prefix so the prefix holds nothing extra.
client_lock() {
    mkdir -p "$(dirname "$PREFIX")" || die "could not create $(dirname "$PREFIX")"
    mkdir "$PREFIX.install-lock" 2>/dev/null \
        || die "could not take $PREFIX.install-lock — another client-only install is running; remove the lock only if none is"
    LOCK=$PREFIX.install-lock
}

# client_only <tag> <target>: install a release for the client-only mode into
# <prefix>/releases/<tag>, verify the attested archive first, then write the
# client-install.json receipt and the cadence-client link. Never touches
# ~/.local/bin/cadence or any existing daemon installation.
client_only_install() {
    CO_TAG=$1
    CO_TARGET=$2
    CO_ASSET=cadence-$CO_TAG-$CO_TARGET.tar.gz
    CO_URL=https://github.com/$REPO/releases/download/$CO_TAG/$CO_ASSET
    CO_DEST=$PREFIX/releases/$CO_TAG

    have gh || die "client-only install needs the GitHub CLI (gh) to verify build provenance"
    if [ -n "$CLIENT_CURRENT" ] && [ "$CO_TAG" != "$CLIENT_CURRENT" ]; then
        release_newer "$CO_TAG" "$CLIENT_CURRENT" \
            || die "$CO_TAG is not newer than the installed client $CLIENT_CURRENT — client-only installs move forward only"
    fi

    say "installing cadence $CO_TAG ($CO_TARGET) as a client-only installation into $CO_DEST"
    fetch "$CO_URL.sha256" "$TMP/$CO_ASSET.sha256"
    expected=$(recorded_sha256 "$TMP/$CO_ASSET.sha256")
    fetch "$CO_URL" "$TMP/$CO_ASSET"
    actual=$(sha256_of "$TMP/$CO_ASSET")
    [ "$actual" = "$expected" ] \
        || die "checksum mismatch for $CO_ASSET: published $expected, downloaded file hashes to $actual — refusing to install"

    # Provenance comes before the archive is unpacked: the attestation binds
    # the archive to this repository, the release workflow and the tag ref.
    gh attestation verify "$TMP/$CO_ASSET" \
        --repo "$REPO" \
        --signer-workflow "$REPO/.github/workflows/ci.yml" \
        --source-ref "refs/tags/$CO_TAG" \
        --deny-self-hosted-runners >/dev/null 2>&1 \
        || die "build-provenance attestation for $CO_ASSET did not verify — refusing to install"

    # Only the three release members may be unpacked, each a regular file.
    tar -tzf "$TMP/$CO_ASSET" >"$TMP/names.raw" || die "could not list $CO_ASSET"
    LC_ALL=C sort "$TMP/names.raw" >"$TMP/names"
    printf 'cadence\ncadence.sha256\nmanifest.json\n' >"$TMP/names.expected"
    cmp -s "$TMP/names" "$TMP/names.expected" \
        || die "$CO_ASSET does not hold exactly cadence, cadence.sha256 and manifest.json — refusing to install"
    tar -tvzf "$TMP/$CO_ASSET" >"$TMP/listing" || die "could not list $CO_ASSET"
    while IFS= read -r line; do
        case "$line" in
            -*) ;;
            *) die "$CO_ASSET holds a member that is not a regular file — refusing to install" ;;
        esac
    done <"$TMP/listing"
    mkdir "$TMP/x"
    tar -xzf "$TMP/$CO_ASSET" -C "$TMP/x" cadence cadence.sha256 manifest.json \
        || die "could not unpack $CO_ASSET"
    for f in cadence cadence.sha256 manifest.json; do
        [ -f "$TMP/x/$f" ] && [ ! -L "$TMP/x/$f" ] \
            || die "$CO_ASSET is missing a regular $f — refusing to install"
    done
    digest=$(recorded_sha256 "$TMP/x/cadence.sha256")
    [ "$(sha256_of "$TMP/x/cadence")" = "$digest" ] \
        || die "the binary in $CO_ASSET does not match its cadence.sha256 — refusing to install"
    source=$(sed -n 's/.*"source_sha"[[:space:]]*:[[:space:]]*"\([0-9a-f]\{40\}\)".*/\1/p' "$TMP/x/manifest.json" | head -n 1)
    [ -n "$source" ] || die "$CO_ASSET manifest names no source commit — refusing to install"
    # The manifest's source commit must be the one the archive attests to.
    gh attestation verify "$TMP/$CO_ASSET" \
        --repo "$REPO" \
        --signer-workflow "$REPO/.github/workflows/ci.yml" \
        --source-ref "refs/tags/$CO_TAG" \
        --source-digest "$source" \
        --deny-self-hosted-runners >/dev/null 2>&1 \
        || die "build-provenance attestation for $CO_ASSET did not verify — refusing to install"
    say "attestation ok: $REPO $CO_TAG $source"

    chmod 0755 "$TMP/x/cadence"
    version_out=$("$TMP/x/cadence" --version) || die "downloaded cadence --version failed"
    case "$version_out" in
        "cadence ${CO_TAG#v}+$source") ;;
        *) die "downloaded cadence --version reports '$version_out', expected cadence ${CO_TAG#v}+$source — refusing to install" ;;
    esac
    # A release without the unified client-only update is never linked as a
    # client. Its native-only `update --help` lacks the marker, so it refuses.
    help_out=$("$TMP/x/cadence" update --help </dev/null 2>&1) \
        && case "$help_out" in
            *"Upgrade a client-only installation"*) ;;
            *) false ;;
        esac \
        || die "$CO_TAG is not capable of client-only installs (its cadence has no client-only update) — choose a newer public release"

    # Commit under the lock: the link and receipt must still be the state the
    # checks saw before the download, or nothing is written.
    seen=$CLIENT_SEEN
    client_lock
    client_check_prefix
    [ "$CLIENT_SEEN" = "$seen" ] \
        || die "the client-only installation changed while $CO_TAG was downloaded — refusing to overwrite it; rerun"

    mkdir -p "$PREFIX/releases"
    if [ -e "$CO_DEST" ] || [ -L "$CO_DEST" ]; then
        [ -d "$CO_DEST" ] && [ ! -L "$CO_DEST" ] \
            && [ -f "$CO_DEST/cadence" ] && [ ! -L "$CO_DEST/cadence" ] \
            && [ "$(sha256_of "$CO_DEST/cadence")" = "$digest" ] \
            || die "$CO_DEST exists but does not hold this release's binary — move it aside and rerun"
    else
        STAGE=$(mktemp -d "$PREFIX/releases/.install-$CO_TAG.XXXXXX") || die "mktemp in $PREFIX/releases failed"
        for f in manifest.json cadence.sha256 cadence; do
            cp "$TMP/x/$f" "$STAGE/$f"
        done
        chmod 0644 "$STAGE/manifest.json" "$STAGE/cadence.sha256"
        chmod 0755 "$STAGE/cadence" "$STAGE"
        mv "$STAGE" "$CO_DEST" || die "could not move the release into $CO_DEST"
        STAGE=
    fi

    tmp_receipt=$PREFIX/.client-install.$$
    client_receipt "$CO_TARGET" >"$tmp_receipt"
    mv -f "$tmp_receipt" "$PREFIX/client-install.json"

    mkdir -p "$BIN_DIR"
    if [ -e "$CLIENT_LINK" ] && [ ! -L "$CLIENT_LINK" ]; then
        die "$CLIENT_LINK exists and is not a symlink — refusing to replace it"
    fi
    tmp_link=$BIN_DIR/.cadence-client.install-$$
    rm -f "$tmp_link"
    ln -s "$CO_DEST/cadence" "$tmp_link"
    mv -f "$tmp_link" "$CLIENT_LINK" || {
        rm -f "$tmp_link"
        die "could not link $CLIENT_LINK"
    }
    say "linked $CLIENT_LINK -> $CO_DEST/cadence"
    "$CLIENT_LINK" --version >/dev/null || die "$CLIENT_LINK --version failed"
}

# ordinary_check_client: an ordinary install never reuses a client-only prefix
# or replaces a link into one. Runs before any download or write.
ordinary_check_client() {
    if [ -e "$PREFIX/client-install.json" ] || [ -L "$PREFIX/client-install.json" ]; then
        die "$PREFIX holds a client-only prefix (client-install.json) — refusing to install an ordinary cadence there"
    fi
    if [ -L "$LINK" ]; then
        linked=$(readlink "$LINK" 2>/dev/null || true)
        case "$linked" in
            */releases/*/cadence)
                owner=${linked%/releases/*/cadence}
                if [ -e "$owner/client-install.json" ] || [ -L "$owner/client-install.json" ]; then
                    die "$LINK points into a client-only prefix ($owner) — refusing to replace it"
                fi
                ;;
        esac
    fi
}

have() { command -v "$1" >/dev/null 2>&1; }

main() {
    REPO=favcrm/cadence
    DEFAULT_BASE_URL=https://github.com/$REPO/releases/download
    BASE_URL=${CADENCE_INSTALL_BASE_URL:-$DEFAULT_BASE_URL}
    API_URL=${CADENCE_INSTALL_API_URL:-https://api.github.com/repos/$REPO}
    VERSION=latest
    PREFIX=
    COMPLETE=
    CLIENT_ONLY=
    BASE_OVERRIDE=
    LOCK=

    while [ $# -gt 0 ]; do
        case "$1" in
            --version | --prefix | --base-url)
                [ $# -ge 2 ] || die "$1 needs a value"
                case "$1" in
                    --version) VERSION=$2 ;;
                    --prefix) PREFIX=$2 ;;
                    --base-url) BASE_URL=$2; BASE_OVERRIDE=1 ;;
                esac
                shift 2
                ;;
            --version=*) VERSION=${1#*=}; shift ;;
            --prefix=*) PREFIX=${1#*=}; shift ;;
            --base-url=*) BASE_URL=${1#*=}; BASE_OVERRIDE=1; shift ;;
            --client-only) CLIENT_ONLY=1; shift ;;
            -h | --help) usage; exit 0 ;;
            --end-of-script) COMPLETE=1; shift ;;
            *) usage >&2; die "unknown argument: $1" ;;
        esac
    done

    [ -n "$COMPLETE" ] || die "install.sh was cut short in transit — download it again"
    [ -n "${HOME:-}" ] || die "HOME is not set"
    if [ -n "$CLIENT_ONLY" ]; then
        # Only the fixed public GitHub source: no mirror and no override.
        [ -z "$BASE_OVERRIDE" ] && [ -z "${CADENCE_INSTALL_BASE_URL:-}${CADENCE_INSTALL_API_URL:-}" ] \
            || die "--client-only installs only from the public GitHub release; --base-url and CADENCE_INSTALL_* overrides are refused"
        [ -n "$PREFIX" ] || PREFIX=${XDG_DATA_HOME:-$HOME/.local/share}/cadence-client
        case "$PREFIX" in
            /*) ;;
            *) PREFIX=$(pwd)/$PREFIX ;;
        esac
        # Normalize so the shell lock (<prefix>.install-lock) names the same
        # path as the updater's: collapse repeated and trailing slashes ("/"
        # stays "/") and refuse . or .. components before any write.
        PREFIX=$(printf '%s' "$PREFIX" | tr -s '/')
        while [ "$PREFIX" != / ] && [ "${PREFIX%/}" != "$PREFIX" ]; do
            PREFIX=${PREFIX%/}
        done
        case "/$PREFIX/" in
            */./* | */../*) die "--prefix cannot contain . or .. components: $PREFIX" ;;
        esac
        BIN_DIR=$HOME/.local/bin
        CLIENT_LINK=$BIN_DIR/cadence-client
        os=$(uname -s)
        arch=$(uname -m)
        case "$arch" in x86_64 | amd64) arch=x86_64 ;; aarch64 | arm64) arch=aarch64 ;; *) die "unsupported CPU: $arch" ;; esac
        if [ "$os" = Darwin ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
            arch=aarch64
        fi
        case "$os-$arch" in
            Linux-x86_64) target=x86_64-linux ;;
            Linux-aarch64) target=aarch64-linux ;;
            Darwin-aarch64) target=aarch64-macos ;;
            Darwin-x86_64) die "unsupported: Intel macOS (client releases exist for Apple silicon)" ;;
            *) die "client-only installs support Ubuntu x86_64/aarch64 and Apple silicon macOS, not $os $arch" ;;
        esac
        CLIENT_TARGET=$target
        client_check_prefix
        TMP=$(mktemp -d "${TMPDIR:-/tmp}/cadence-install.XXXXXX") || die "mktemp failed"
        STAGE=
        trap cleanup EXIT
        trap 'exit 130' INT
        trap 'exit 143' TERM
        if [ "$VERSION" = latest ]; then
            fetch "$API_URL/releases/latest" "$TMP/latest.json"
            VERSION=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$TMP/latest.json" | head -n 1)
            [ -n "$VERSION" ] || die "could not read the latest release tag"
        fi
        case "$VERSION" in v*) TAG=$VERSION ;; *) TAG=v$VERSION ;; esac
        case "$TAG" in
            v | *[!0-9A-Za-z.+_-]* | *..*) die "not a release tag: $VERSION" ;;
        esac
        release_key "$TAG" >/dev/null \
            || die "$TAG is not a stable vMAJOR.MINOR.PATCH release tag — client-only installs refuse prerelease and build-metadata tags"
        client_only_install "$TAG" "$target"
        say "ok: $CLIENT_LINK --version"
        exit 0
    fi
    if [ -z "$PREFIX" ]; then
        PREFIX=${XDG_DATA_HOME:-$HOME/.local/share}/cadence
    fi
    case "$PREFIX" in
        /*) ;;
        *) PREFIX=$(pwd)/$PREFIX ;;
    esac
    BIN_DIR=$HOME/.local/bin
    LINK=$BIN_DIR/cadence
    ordinary_check_client

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
    # Refuse a mislabeled release before installing it or moving the active
    # link. Checksums establish byte integrity, not the requested version.
    chmod 0755 "$TMP/x/cadence"
    version_out=$("$TMP/x/cadence" --version) || die "downloaded cadence --version failed"
    case "$version_out" in
        "cadence ${TAG#v}+"*) ;;
        *) die "downloaded cadence --version reports '$version_out', expected cadence ${TAG#v}+<commit> — refusing to install" ;;
    esac

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
