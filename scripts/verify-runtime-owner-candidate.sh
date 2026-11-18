#!/usr/bin/env bash
# Fetch and verify the exact main-CI pair without promoting or installing it.
set -euo pipefail
if [[ $# != 3 || ! $1 =~ ^[0-9a-f]{40}$ || ! $2 =~ ^[1-9][0-9]*$ || $3 != /* ]]; then
    echo 'Usage: bash scripts/verify-runtime-owner-candidate.sh <source-sha> <ci-run-id> <new-absolute-output-dir>' >&2
    exit 2
fi

umask 077
source_sha=$1
run_id=$2
out=$3
repo=favcrm/cadence
scripts=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
mkdir -- "$out"
python3 "$scripts/delivery-candidate.py" prepare --repo "$repo" --run-id "$run_id" \
    --dest "$out/supervisor" >/dev/null
receipt="$out/supervisor/candidate.json"
if ! attempt=$(jq -er --arg sha "$source_sha" 'select(.source_sha == $sha) | .ci_run_attempt' "$receipt"); then
    echo 'Verified supervisor CI source does not match the requested source' >&2
    exit 1
fi

if ! gh run download "$run_id" --repo "$repo" --dir "$out/owner" \
    --name "cadence-runtime-owner-$source_sha-x86_64-linux"; then
    echo 'The verified supervisor CI run has no downloadable runtime owner companion' >&2
    exit 1
fi
verified=$(bash "$scripts/verify-runtime-owner.sh" "$out/owner" "$source_sha" "$run_id" "$attempt")
jq -e --arg digest "${verified%% *}" '{schema: 1, supervisor: ., owner: {sha256: $digest}}' \
    "$receipt" > "$out/.verified-pair.json"
mv -- "$out/.verified-pair.json" "$out/verified-pair.json"
cat -- "$out/verified-pair.json"
