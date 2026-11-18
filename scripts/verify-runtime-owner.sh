#!/usr/bin/env bash
# Verify the companion against an independently verified production candidate.
set -euo pipefail
if [[ $# != 4 || ! $2 =~ ^[0-9a-f]{40}$ || ! $3 =~ ^[1-9][0-9]*$ || ! $4 =~ ^[1-9][0-9]*$ ]]; then
    echo 'Usage: bash scripts/verify-runtime-owner.sh <owner-dir> <source-sha> <ci-run-id> <ci-run-attempt>' >&2
    exit 2
fi

cd -- "$1"
repo=favcrm/cadence
owner=cadence-runtime-owner
for member in "$owner" "$owner.sha256" owner-attestation.jsonl; do
    if [[ ! -f $member || -L $member ]]; then
        echo "Missing or unsafe runtime owner member: $member" >&2
        exit 1
    fi
done
size=$(stat -c '%s' "$owner")
if [[ $size -eq 0 || $size -gt 134217728 ]]; then
    echo 'Runtime owner exceeds the measured executable bound or is empty' >&2
    exit 1
fi
checksum=$(sha256sum "$owner")
if [[ $(cat "$owner.sha256") != "$checksum" ]]; then
    echo 'Runtime owner checksum mismatch' >&2
    exit 1
fi

# The exact CI workflow SAN is stricter than a signer-workflow prefix. Only
# statements returned by gh *after* signature and source verification are used.
proof=$(gh attestation verify "$owner" --repo "$repo" --bundle owner-attestation.jsonl \
    --cert-identity "https://github.com/$repo/.github/workflows/ci.yml@refs/heads/main" \
    --signer-digest "$2" --source-ref refs/heads/main --source-digest "$2" \
    --deny-self-hosted-runners --format json)
if ! jq -e --arg name "$owner" --arg digest "${checksum%% *}" \
    --arg invocation "https://github.com/$repo/actions/runs/$3/attempts/$4" '
    any(.[].verificationResult.statement;
        .predicate.runDetails.metadata.invocationId == $invocation and
        any(.subject[]; .name == $name and .digest.sha256 == $digest))
' <<< "$proof" >/dev/null; then
    echo 'Runtime owner attestation does not match the promoted CI run and attempt' >&2
    exit 1
fi
printf '%s\n' "$checksum"
