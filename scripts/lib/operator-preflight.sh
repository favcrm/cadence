#!/usr/bin/env bash
# Shared, side-effect-free preflight functions for pinned operator scripts.
# Source this file; call only the checks needed by the script before mutation.

_preflight_refuse() {
    printf 'preflight: %s\n' "$1" >&2
    return 1
}

preflight_gh_auth() {
    command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1 \
        || _preflight_refuse 'GitHub CLI is not authenticated'
}

preflight_cf_auth() {
    (($#)) || { _preflight_refuse 'Cloudflare read-auth probe is not configured'; return 1; }
    command -v "$1" >/dev/null 2>&1 && "$@" >/dev/null 2>&1 \
        || _preflight_refuse 'Cloudflare CLI is missing or not authenticated'
}

preflight_sha() {
    local expected=${1-} actual=${2-}
    [[ $expected =~ ^[0-9a-fA-F]{40}$ ]] \
        || { _preflight_refuse 'expected SHA must be a full 40-character hex commit'; return 1; }
    [[ $actual =~ ^[0-9a-fA-F]{40}$ ]] \
        || { _preflight_refuse 'live SHA must be a full 40-character hex commit'; return 1; }
    [[ ${expected,,} == "${actual,,}" ]] \
        || _preflight_refuse 'live SHA does not match expected SHA'
}

preflight_digest_set() {
    [[ ${1-} =~ [^[:space:]] ]] || _preflight_refuse 'expected digest is missing'
}

preflight_state_dir() {
    local state_dir=${1-}
    local canonical_state default_state
    [[ -n $state_dir && $state_dir == /* ]] \
        || { _preflight_refuse 'Cadence state directory must be explicitly pinned to an absolute path'; return 1; }
    canonical_state=$(realpath -m -- "$state_dir" 2>/dev/null) \
        || { _preflight_refuse 'Cadence state directory could not be canonicalized'; return 1; }
    default_state=$(realpath -m -- "${HOME:-/}/.local/state/cadence" 2>/dev/null) \
        || { _preflight_refuse 'default Cadence state directory could not be canonicalized'; return 1; }
    [[ $canonical_state != "$default_state" ]] \
        || _preflight_refuse 'Cadence state directory must not be the default production state'
}

preflight_probe() {
    (($#)) || { _preflight_refuse 'tenant probe command is missing'; return 1; }
    "$@" >/dev/null 2>&1 \
        || _preflight_refuse 'tenant did not answer the configured probe'
}
