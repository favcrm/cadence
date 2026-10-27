#!/usr/bin/env bash
# Complete, runnable fixture recipe for the `native-oci-agent/1` protected
# runtime.
#
# It builds a real digest-pinned fixture image from explicitly supplied local
# artifacts (no network fetch, no floating tag), then runs the profile
# generator against the exact same staged bytes so every image/executable/graph
# pin in the profile is a calculated sha256 — never a placeholder. It never
# starts a container, never deploys and never touches production paths.
#
# Usage (root):
#   build_fixture.sh \
#     --base registry.example/node@sha256:<64 lowercase hex> \
#     --node /path/to/node --pi /path/to/pi.mjs \
#     --pi-graph /path/to/pi-1.4.0/node_modules \
#     --model-bridge infra/native-protected-runtime/model-bridge.mjs \
#     --model-sdk infra/native-protected-runtime/model-sdk \
#     --guest-model claude-opus-5-high --guest-effort high --guest-tools read,bash \
#     [--guest-provider devin] [--guest-max-output 8192] [--guest-max-context 1000000] \
#     [--native-model /path/to/native-model.json] \
#     --observer observer/observer.mjs --material /path/to/material \
#     (--build-seal --node-headers /path/to/node/include/node |
#      --seal-addon /path/to/prebuilt/cadence_seal.node) \
#     --installer /path/to/pinned-installer \
#     --supervisor /path/to/pinned-supervisor \
#     --owner /path/to/cadence-runtime-owner \
#     --seccomp seccomp.json --approved /path/to/approved-registry.json \
#     --policy-digest sha256:<64 hex> \
#     --issuer-kid fixture-issuer [--issuer-private-key <root 0600 Ed25519 PKCS#8>] \
#     [--connection-schemas /path/to/contracts/cadence-core/v1] \
#     --workspace-id workspace-fixture --core-id core-fixture \
#     --lineage-reference lineage-fixture --operator-origin https://operator.invalid \
#     --supervisor-uid 21000 --supervisor-gid 21000 \
#     --uid-start 21001 --uid-end 21999 \
#     --image-tag localhost:3122/cadence-native-fixture:local --publish-local \
#     --work /var/tmp/cadence-native-fixture-build \
#     --out /var/lib/cadence/runtime-owner
#
# `--out` must be a fresh/empty directory. The script asserts the required
# public/private split: profile dir 0755 root, profile.json/seccomp.json 0644,
# state/ 0700, owner.key 0600, run/ 0770 root:supervisor_gid (the supervisor
# binds its broker-fence socket there), and /run/cadence-supervisor 0770
# root:supervisor. `--pi` defaults to this repo's reviewed guest bootstrap;
# `--model-bridge`/`--model-sdk` default to the model-turn owner's sources.
# The guest session config is generated into the immutable material root and
# must match the operator's native-model catalog/profile pins. The reviewed NAPI
# seal addon is built (or supplied) into the staged rootfs BEFORE `docker build`
# and pinned from those same bytes; there is no pre-exec seal executable.
set -euo pipefail

die() { printf 'build_fixture: %s\n' "$*" >&2; exit 1; }

[ "${EUID}" -eq 0 ] || die "must run as root so ownership/pins are real"

BASE=""; NODE=""; PI=""; PI_GRAPH=""; OBSERVER=""; MATERIAL=""; INSTALLER=""
SEAL_ADDON=""; BUILD_SEAL=""; NODE_HEADERS=""
SUPERVISOR=""; OWNER=""; SECCOMP=""; APPROVED=""; POLICY_DIGEST=""
ISSUER_KID=""; ISSUER_PRIVATE_KEY=""; CONNECTION_SCHEMAS=""; WORKSPACE_ID=""; CORE_ID=""; LINEAGE=""
OPERATOR_ORIGIN=""; SUPERVISOR_UID=""; SUPERVISOR_GID=""; UID_START=""
UID_END=""; IMAGE_TAG=""; WORK=""; OUT=""; MAX_RUNTIMES="1"
PUBLISH_LOCAL=""
DATABASE_EPOCH="1"; INSTALLER_GENERATION=""
MODEL_BRIDGE=""; MODEL_SDK=""; NATIVE_MODEL=""; NATIVE_MODEL_TOOLS=""
GUEST_PROVIDER="devin"; GUEST_MODEL=""; GUEST_EFFORT=""; GUEST_MAX_OUTPUT="8192"
GUEST_MAX_CONTEXT="1000000"; GUEST_TOOLS=""

while [ $# -gt 0 ]; do
  case "$1" in
    --base) BASE="$2"; shift 2 ;;
    --node) NODE="$2"; shift 2 ;;
    --pi) PI="$2"; shift 2 ;;
    --pi-graph) PI_GRAPH="$2"; shift 2 ;;
    --model-bridge) MODEL_BRIDGE="$2"; shift 2 ;;
    --model-sdk) MODEL_SDK="$2"; shift 2 ;;
    --native-model) NATIVE_MODEL="$2"; shift 2 ;;
    --native-model-tools) NATIVE_MODEL_TOOLS="$2"; shift 2 ;;
    --guest-provider) GUEST_PROVIDER="$2"; shift 2 ;;
    --guest-model) GUEST_MODEL="$2"; shift 2 ;;
    --guest-effort) GUEST_EFFORT="$2"; shift 2 ;;
    --guest-max-output) GUEST_MAX_OUTPUT="$2"; shift 2 ;;
    --guest-max-context) GUEST_MAX_CONTEXT="$2"; shift 2 ;;
    --guest-tools) GUEST_TOOLS="$2"; shift 2 ;;
    --observer) OBSERVER="$2"; shift 2 ;;
    --material) MATERIAL="$2"; shift 2 ;;
    --build-seal) BUILD_SEAL=1; shift ;;
    --node-headers) NODE_HEADERS="$2"; shift 2 ;;
    --seal-addon) SEAL_ADDON="$2"; shift 2 ;;
    --installer) INSTALLER="$2"; shift 2 ;;
    --supervisor) SUPERVISOR="$2"; shift 2 ;;
    --owner) OWNER="$2"; shift 2 ;;
    --seccomp) SECCOMP="$2"; shift 2 ;;
    --approved) APPROVED="$2"; shift 2 ;;
    --policy-digest) POLICY_DIGEST="$2"; shift 2 ;;
    --issuer-kid) ISSUER_KID="$2"; shift 2 ;;
    --issuer-private-key) ISSUER_PRIVATE_KEY="$2"; shift 2 ;;
    --connection-schemas) CONNECTION_SCHEMAS="$2"; shift 2 ;;
    --workspace-id) WORKSPACE_ID="$2"; shift 2 ;;
    --core-id) CORE_ID="$2"; shift 2 ;;
    --lineage-reference) LINEAGE="$2"; shift 2 ;;
    --operator-origin) OPERATOR_ORIGIN="$2"; shift 2 ;;
    --supervisor-uid) SUPERVISOR_UID="$2"; shift 2 ;;
    --supervisor-gid) SUPERVISOR_GID="$2"; shift 2 ;;
    --uid-start) UID_START="$2"; shift 2 ;;
    --uid-end) UID_END="$2"; shift 2 ;;
    --database-epoch) DATABASE_EPOCH="$2"; shift 2 ;;
    --installer-generation) INSTALLER_GENERATION="$2"; shift 2 ;;
    --max-runtimes) MAX_RUNTIMES="$2"; shift 2 ;;
    --image-tag) IMAGE_TAG="$2"; shift 2 ;;
    --publish-local) PUBLISH_LOCAL="1"; shift ;;
    --work) WORK="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) die "unknown argument $1" ;;
  esac
done

HERE="$(cd "$(dirname "$0")/.." && pwd)"
CONNECTION_SCHEMAS="${CONNECTION_SCHEMAS:-$HERE/../../contracts/cadence-core/v1}"
[ -n "$PI" ] || PI="$HERE/guest/pi.mjs"
[ -n "$MODEL_BRIDGE" ] || MODEL_BRIDGE="$HERE/model-bridge.mjs"
[ -n "$MODEL_SDK" ] || MODEL_SDK="$HERE/model-sdk"
[ -n "$INSTALLER" ] || INSTALLER="$OWNER"

for binding in BASE NODE PI PI_GRAPH OBSERVER MATERIAL INSTALLER SUPERVISOR OWNER \
               SECCOMP APPROVED POLICY_DIGEST ISSUER_KID WORKSPACE_ID \
               CORE_ID LINEAGE OPERATOR_ORIGIN SUPERVISOR_UID SUPERVISOR_GID \
               UID_START UID_END IMAGE_TAG WORK OUT MODEL_BRIDGE MODEL_SDK \
               GUEST_MODEL GUEST_TOOLS; do
  [ -n "${!binding}" ] || die "missing --${binding,,}"
done
if [ -n "$BUILD_SEAL" ]; then
  [ -n "$NODE_HEADERS" ] || die "--build-seal requires --node-headers"
  [ -d "$NODE_HEADERS" ] && [ ! -L "$NODE_HEADERS" ] || die "Node headers must be a real directory: $NODE_HEADERS"
  [ -z "$SEAL_ADDON" ] || die "--build-seal and --seal-addon are mutually exclusive"
elif [ -z "$SEAL_ADDON" ]; then
  die "missing seal addon: pass --build-seal --node-headers DIR or --seal-addon FILE"
fi

[[ "$BASE" =~ @sha256:[0-9a-f]{64}$ ]] || die "--base must be digest-pinned (repo@sha256:<64 lowercase hex>)"
[[ "$POLICY_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || die "--policy-digest must be sha256:<64 lowercase hex>"
[[ "$IMAGE_TAG" =~ :[^/:]+$ ]] || die "--image-tag must include an explicit tag"
if [ -n "$PUBLISH_LOCAL" ]; then
  [[ "$IMAGE_TAG" =~ ^(localhost|127\.0\.0\.1):31[1-9][0-9]/[a-z0-9][a-z0-9._/-]*:[A-Za-z0-9_][A-Za-z0-9_.-]*$ ]] \
    || die "--publish-local requires a loopback registry on port 3110-3199"
fi
for exe in "$NODE" "$PI" "$OBSERVER" "$INSTALLER" "$SUPERVISOR" "$OWNER"; do
  [ -f "$exe" ] && [ ! -L "$exe" ] || die "not a regular file: $exe"
done
if [ -n "$SEAL_ADDON" ]; then
  [ -f "$SEAL_ADDON" ] && [ ! -L "$SEAL_ADDON" ] || die "not a regular file: $SEAL_ADDON"
fi
[ -d "$MATERIAL" ] && [ ! -L "$MATERIAL" ] || die "material root must be a real directory"
[ -d "$PI_GRAPH" ] && [ ! -L "$PI_GRAPH" ] || die "Pi SDK graph must be a real node_modules directory"
[ -d "$MODEL_SDK" ] && [ ! -L "$MODEL_SDK" ] || die "model SDK must be a real directory"
[[ "$GUEST_PROVIDER" =~ ^[a-z][a-z0-9_-]{1,31}$ ]] || die "--guest-provider is malformed"
[[ "$GUEST_MODEL" =~ ^[A-Za-z0-9._-]{1,128}$ ]] || die "--guest-model is malformed"
[[ "$GUEST_MAX_OUTPUT" =~ ^[1-9][0-9]*$ ]] || die "--guest-max-output is malformed"
[[ "$GUEST_MAX_CONTEXT" =~ ^[1-9][0-9]*$ ]] || die "--guest-max-context is malformed"
if [ -n "$GUEST_EFFORT" ]; then
  case "$GUEST_EFFORT" in
    off|minimal|low|medium|high|xhigh|max) : ;;
    *) die "--guest-effort must be a Pi thinking level" ;;
  esac
fi
[ -f "$SECCOMP" ] && [ -f "$APPROVED" ] || die "seccomp/approved registry inputs are missing"
[ -d "$OUT" ] && [ -z "$(ls -A "$OUT" 2>/dev/null)" ] || die "--out must be a fresh/empty directory"

if [ -z "$INSTALLER_GENERATION" ]; then
  INSTALLER_GENERATION="$(openssl rand -hex 16)"
fi
[[ "$INSTALLER_GENERATION" =~ ^[0-9a-f]{32}$ ]] || die "--installer-generation must be 32 lowercase hex"

install -d -m 0755 "$WORK"
ROOTFS="$WORK/rootfs"
rm -rf "$ROOTFS"
install -d -m 0755 \
  "$ROOTFS/opt/cadence/node/bin" \
  "$ROOTFS/opt/cadence/run"
install -m 0755 "$NODE" "$ROOTFS/opt/cadence/node/bin/node"
install -m 0555 "$PI" "$ROOTFS/opt/cadence/node/pi.mjs"
install -m 0555 "$OBSERVER" "$ROOTFS/opt/cadence/node/observer.mjs"
# The reviewed NAPI seal addon the observer requires. It is staged into the
# rootfs BEFORE `docker build` so the image and the profile pin the same bytes.
if [ -n "$BUILD_SEAL" ]; then
  python3 "$(dirname "$0")/generate_profile.py" --stage-seal \
    --image-root "$ROOTFS" \
    --node-headers "$NODE_HEADERS"
else
  install -m 0555 "$SEAL_ADDON" "$ROOTFS/opt/cadence/node/cadence_seal.node"
fi
SEAL_ADDON="$ROOTFS/opt/cadence/node/cadence_seal.node"
[ -f "$SEAL_ADDON" ] && [ ! -L "$SEAL_ADDON" ] \
  || die "seal addon was not staged into the image root"
# The pinned Pi 1.4 SDK graph the guest bootstrap imports by bare specifier.
cp -a "$PI_GRAPH" "$ROOTFS/opt/cadence/node/node_modules"
# npm's .bin links are CLI shims, not the imported SDK graph. The guest has
# its own fixed entrypoint; omit these unused links and retain strict graph
# measurement's refusal of every remaining symlink.
rm -rf "$ROOTFS/opt/cadence/node/node_modules/.bin"
find "$ROOTFS/opt/cadence/node/node_modules" -type d -exec chmod 0555 {} +
find "$ROOTFS/opt/cadence/node/node_modules" -type f -exec chmod 0444 {} +
cp -a "$MATERIAL" "$ROOTFS/opt/cadence/material"
install -d -m 0555 "$ROOTFS/opt/cadence/material/connection-schemas"
for schema in connection-snapshot-args.schema.json \
              connection-stage-send-args.schema.json \
              connection-quote-args.schema.json \
              connection-invoke.schema.json \
              connection-result-args.schema.json \
              connection-request-args.schema.json; do
  [ -f "$CONNECTION_SCHEMAS/$schema" ] && [ ! -L "$CONNECTION_SCHEMAS/$schema" ] \
    || die "missing generated connection schema: $CONNECTION_SCHEMAS/$schema"
  install -m 0444 "$CONNECTION_SCHEMAS/$schema" \
    "$ROOTFS/opt/cadence/material/connection-schemas/$schema"
done
install -m 0555 "$MODEL_BRIDGE" "$ROOTFS/opt/cadence/material/model-bridge.mjs"
cp -a "$MODEL_SDK" "$ROOTFS/opt/cadence/material/model-sdk"
install -d -m 0555 "$ROOTFS/opt/cadence/material/guest"
python3 - "$ROOTFS/opt/cadence/material/guest/session.json" \
  "$GUEST_PROVIDER" "$GUEST_MODEL" "$GUEST_EFFORT" "$GUEST_MAX_OUTPUT" \
  "$GUEST_MAX_CONTEXT" "$GUEST_TOOLS" <<'PYJSON'
import json, sys
out, provider, model, effort, max_output, max_context, tools = sys.argv[1:8]
payload = {
    "v": 1,
    "provider": provider,
    "model_uid": model,
    "effort": effort or None,
    "max_output_tokens": int(max_output),
    "max_context_tokens": int(max_context),
    "tools": [name for name in tools.split(",") if name],
}
with open(out, "w", encoding="utf-8") as handle:
    json.dump(payload, handle, sort_keys=True, separators=(",", ":"))
    handle.write("\n")
PYJSON
chmod 0444 "$ROOTFS/opt/cadence/material/guest/session.json"
find "$ROOTFS/opt/cadence/material" -type d -exec chmod 0555 {} +
find "$ROOTFS/opt/cadence/material" -type f -exec chmod 0444 {} +

docker build \
  --pull=false \
  -f "$(dirname "$0")/fixture.Dockerfile" \
  --build-arg "BASE=$BASE" \
  -t "$IMAGE_TAG" \
  "$WORK"
if [ -n "$PUBLISH_LOCAL" ]; then
  docker push "$IMAGE_TAG"
fi
# A config ID is not an OCI manifest digest or a resolvable repository
# reference. Pin only the actual RepoDigest recorded by the registry.
REPOSITORY="${IMAGE_TAG%:*}"
REPO_DIGESTS="$(docker image inspect --format '{{json .RepoDigests}}' "$IMAGE_TAG")"
IMAGE_REF="$(python3 - "$REPOSITORY" "$REPO_DIGESTS" <<'PYREF'
import json, sys
repository, encoded = sys.argv[1:3]
matches = [
    value for value in (json.loads(encoded) or [])
    if isinstance(value, str) and value.startswith(repository + "@sha256:")
]
if len(matches) != 1:
    raise SystemExit(
        "build_fixture: the image has no unique registry RepoDigest; publish "
        "through the reviewed image pipeline or use --publish-local with a "
        "loopback fixture registry"
    )
print(matches[0])
PYREF
)"

install -d -m 0755 "$OUT"
parent="$(dirname "$OUT")"
while [ "$parent" != "/" ]; do
  parent_uid="$(stat -c '%u' "$parent")"
  parent_mode="$(stat -c '%a' "$parent")"
  [ "$parent_uid" = "0" ] || die "profile parent is not root-owned: $parent"
  [ $(( 8#$parent_mode & 8#022 )) -eq 0 ] \
    || die "profile parent is group/other writable: $parent"
  parent="$(dirname "$parent")"
done
ISSUER_ARGS=()
if [ -n "$ISSUER_PRIVATE_KEY" ]; then
  ISSUER_ARGS=(--issuer-private-key "$ISSUER_PRIVATE_KEY")
fi
python3 "$(dirname "$0")/generate_profile.py" \
  --image-root "$ROOTFS" \
  --out "$OUT" \
  --image-ref "$IMAGE_REF" \
  --workspace-id "$WORKSPACE_ID" \
  --core-id "$CORE_ID" \
  --lineage-reference "$LINEAGE" \
  --database-epoch "$DATABASE_EPOCH" \
  --supervisor-uid "$SUPERVISOR_UID" \
  --supervisor-gid "$SUPERVISOR_GID" \
  --supervisor-exe "$SUPERVISOR" \
  --installer-exe "$INSTALLER" \
  --installer-generation "$INSTALLER_GENERATION" \
  --owner-exe "$OWNER" \
  --issuer-kid "$ISSUER_KID" \
  "${ISSUER_ARGS[@]}" \
  --seccomp "$SECCOMP" \
  --approved-registry "$APPROVED" \
  --policy-digest "$POLICY_DIGEST" \
  --uid-start "$UID_START" \
  --uid-end "$UID_END" \
  --operator-origin "$OPERATOR_ORIGIN" \
  --max-runtimes "$MAX_RUNTIMES"

# Public/private split: the supervisor (uid 21000) must read the public
# profile and seccomp document, never the owner key or the authority DB.
chmod 0755 "$OUT"
chmod 0644 "$OUT/profile.json" "$OUT/seccomp.json"
chmod 0700 "$OUT/state"
chmod 0600 "$OUT/owner.key"
if [ -f "$OUT/issuer.key" ]; then chmod 0600 "$OUT/issuer.key"; fi
# The supervisor must be able to bind its own broker-fence socket here.
chown root:"$SUPERVISOR_GID" "$OUT/run"
chmod 0770 "$OUT/run"
if [ -n "$NATIVE_MODEL" ]; then
  install -o 0 -g 0 -m 0644 "$NATIVE_MODEL" "$OUT/native-model.json"
  if [ -n "$NATIVE_MODEL_TOOLS" ]; then
    [ -f "$NATIVE_MODEL_TOOLS" ] && [ ! -L "$NATIVE_MODEL_TOOLS" ] \
      || die "--native-model-tools must be a regular file"
    install -o 0 -g 0 -m 0644 "$NATIVE_MODEL_TOOLS" "$OUT/native-model-tools.json"
  fi
  # The model-turn owner requires the pinned declaration file and its digest in
  # the route profile. Verify the operator's pair before this tree is used.
  python3 - "$OUT/native-model.json" "$OUT" <<'PYCHECK'
import hashlib, json, os, sys
profile_path, out = sys.argv[1], sys.argv[2]
with open(profile_path, "r", encoding="utf-8") as handle:
    document = json.load(handle)
routes = document.get("routes")
if not isinstance(routes, dict) or not routes:
    raise SystemExit("native-model.json has no routes")
for name, route in routes.items():
    path = route.get("approved_tools_path")
    digest = route.get("approved_tools_digest")
    if not isinstance(path, str) or not isinstance(digest, str):
        raise SystemExit(f"route {name}: approved_tools_path/digest are required")
    if not path.startswith(out.rstrip("/") + "/"):
        raise SystemExit(f"route {name}: approved_tools_path must live in the profile dir")
    if not os.path.isfile(path) or os.path.islink(path):
        raise SystemExit(f"route {name}: approved tools file is missing")
    actual = "sha256:" + hashlib.sha256(open(path, "rb").read()).hexdigest()
    if actual != digest:
        raise SystemExit(f"route {name}: approved_tools_digest does not match the file")
PYCHECK
fi
chmod 0600 "$OUT/state/runtime-owner.sqlite" 2>/dev/null || true
install -d -m 0770 -o 0 -g "$SUPERVISOR_GID" /run/cadence-supervisor

printf 'profile=%s\nimage_ref=%s\ninstaller=%s\ninstaller_generation=%s\n' \
  "$OUT/profile.json" "$IMAGE_REF" "$INSTALLER" "$INSTALLER_GENERATION"
