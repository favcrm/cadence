#!/bin/sh
# CAD-433: E2E acceptance tier A — the whole MVP journey in one scripted
# run, with fake providers and a headless board.
#
#   scripts/e2e/mvp.sh [--no-build]
#
# 1. builds the SPA and `cargo build --release --locked --features ui`
#    (skipped with --no-build: the existing target/release/cadence and
#    ui/dist are used as they are);
# 2. packs that binary exactly as the release workflow's release-build
#    job does (cadence, cadence.sha256, manifest.json in
#    cadence-<tag>-<target>.tar.gz plus its .sha256) into a local
#    download dir;
# 3. installs the pinned headless-browser driver (tests/e2e, pnpm);
# 4. runs the journey: `cargo test --features e2e --test e2e_mvp`
#    (tests/e2e_mvp.rs), which installs that release with
#    scripts/install.sh from file:// into a sandbox under /tmp and walks
#    the eight MVP use cases.
#
# Needs: cargo, pnpm, node, python3, git, curl, tar, sha256sum, and
# Google Chrome (or E2E_CHROME=<browser binary>). The journey itself
# downloads nothing and writes nothing outside its sandbox and $E2E_OUT
# (the board steps run with the sandbox's HOME and XDG dirs). The build
# steps before it use the caller's cargo and pnpm caches as any build
# does.
#
# Evidence (screenshots, the daemon's event log and logs, the fakes'
# logs, acceptance.json) lands in $E2E_OUT, default target/e2e.

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
BUILD=1
for arg in "$@"; do
    case "$arg" in
        --no-build) BUILD=0 ;;
        -h | --help) sed -n '2,25p' "$0"; exit 0 ;;
        *) echo "mvp.sh: unknown argument $arg" >&2; exit 2 ;;
    esac
done

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64) TARGET=x86_64-linux ;;
    Linux-aarch64) TARGET=aarch64-linux ;;
    *) echo "mvp.sh: the journey runs on Linux (the master's Landlock confinement)" >&2; exit 2 ;;
esac

OUT=${E2E_OUT:-$ROOT/target/e2e}
rm -rf "$OUT"
mkdir -p "$OUT"

if [ "$BUILD" = 1 ]; then
    (cd "$ROOT/ui" && pnpm install --frozen-lockfile && pnpm build)
    (cd "$ROOT" && cargo build --release --locked --features ui)
fi
BIN=$ROOT/target/release/cadence
[ -x "$BIN" ] || { echo "mvp.sh: no $BIN — run without --no-build" >&2; exit 1; }

# The local release, laid out like GitHub Releases: <dl>/<tag>/<asset>.
VERSION=$("$BIN" --version)
TAG=v$(printf '%s' "$VERSION" | sed -n 's/^cadence \([^+]*\)+.*/\1/p')
[ "$TAG" != v ] || { echo "mvp.sh: cannot read a version from '$VERSION'" >&2; exit 1; }
SHA=$(printf '%s' "$VERSION" | sed -n 's/^[^+]*+//p')
STAGE=$OUT/release/stage
DL=$OUT/release/dl
mkdir -p "$STAGE" "$DL/$TAG"
cp "$BIN" "$STAGE/cadence"
chmod 0755 "$STAGE/cadence"
digest=$(sha256sum "$STAGE/cadence" | cut -d' ' -f1)
printf '%s  cadence\n' "$digest" >"$STAGE/cadence.sha256"
printf '{"source_sha":"%s","version":"%s","target":"%s","features":["ui"],"sha256":"%s","local":true}\n' \
    "$SHA" "$TAG" "$TARGET" "$digest" >"$STAGE/manifest.json"
ASSET=cadence-$TAG-$TARGET.tar.gz
tar -czf "$DL/$TAG/$ASSET" -C "$STAGE" cadence cadence.sha256 manifest.json
(cd "$DL/$TAG" && printf '%s  %s\n' "$(sha256sum "$ASSET" | cut -d' ' -f1)" "$ASSET" >"$ASSET.sha256")
rm -rf "$STAGE"

(cd "$ROOT/tests/e2e" && pnpm install --frozen-lockfile)

cd "$ROOT"
CADENCE_E2E_DIST=$DL \
CADENCE_E2E_TAG=$TAG \
CADENCE_E2E_VERSION=$VERSION \
CADENCE_E2E_ARTIFACTS=$OUT \
    cargo test --locked --features e2e --test e2e_mvp -- --nocapture --test-threads 1
