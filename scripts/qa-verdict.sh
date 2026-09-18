#!/usr/bin/env bash
# qa-verdict.sh — bind a reviewer's verdict to the exact PR head as a
# `qa-verdict` commit status, or read that status back before a merge.
# usage: qa-verdict.sh <pr-number> <pass|blocked> --note <abs path of the verdict note>
#                      [--repo owner/name] [--sha <sha>]
#        qa-verdict.sh --check <pr-number> [--repo owner/name]
# The status sits on one SHA. A later push moves the PR head and the status
# does not follow, so a verdict never covers code the reviewer did not see.
# Exit: 0 posted / check is `success` · 1 refused / check is anything else · 2 usage.
set -euo pipefail

CONTEXT="qa-verdict"
ME="$(basename "$0")"

usage() {
  cat >&2 <<EOF
usage: $ME <pr-number> <pass|blocked> --note <abs path of the verdict note> [--repo owner/name] [--sha <sha>]
       $ME --check <pr-number> [--repo owner/name]
EOF
  exit 2
}

refuse() {
  echo "$ME: $*" >&2
  exit 1
}

# Every GitHub call goes through here; tests put a fake `gh` first on PATH.
gh_call() {
  gh "$@"
}

head_sha() {
  local sha
  sha="$(gh_call pr view "$pr" --repo "$repo" --json headRefOid --jq .headRefOid)"
  [[ "$sha" =~ ^[0-9a-f]{40}$ ]] || refuse "could not resolve the head SHA of PR #$pr in $repo (got '$sha')"
  printf '%s\n' "$sha"
}

check=0 pr="" verdict="" note="" repo="" want_sha=""
positional=()
while [ $# -gt 0 ]; do
  case "$1" in
    --check) check=1; shift ;;
    --note|--repo|--sha)
      [ $# -ge 2 ] || usage
      case "$1" in
        --note) note="$2" ;;
        --repo) repo="$2" ;;
        --sha) want_sha="$2" ;;
      esac
      shift 2 ;;
    -h|--help) usage ;;
    -*) echo "$ME: unknown option '$1'" >&2; usage ;;
    *) positional+=("$1"); shift ;;
  esac
done

if [ "$check" -eq 1 ]; then
  if [ "${#positional[@]}" -ne 1 ] || [ -n "$note" ] || [ -n "$want_sha" ]; then usage; fi
else
  [ "${#positional[@]}" -eq 2 ] || usage
  verdict="${positional[1]}"
fi
pr="${positional[0]}"
[[ "$pr" =~ ^[1-9][0-9]*$ ]] || { echo "$ME: bad PR number '$pr'" >&2; usage; }

if [ -z "$repo" ]; then
  repo="$(gh_call repo view --json nameWithOwner --jq .nameWithOwner)"
fi
[[ "$repo" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] || refuse "bad repo '$repo' (want owner/name)"

if [ "$check" -eq 1 ]; then
  sha="$(head_sha)"
  # The combined status carries the latest state of each context.
  state="$(gh_call api "repos/$repo/commits/$sha/status?per_page=100" \
    --jq "[.statuses[] | select(.context == \"$CONTEXT\")][0].state // \"none\"")"
  echo "$CONTEXT: ${state:-none} — PR #$pr head ${sha:0:7} ($repo)"
  if [ "$state" != "success" ]; then
    echo "$ME: no passing verdict on this head; a new push needs a new review" >&2
    exit 1
  fi
  exit 0
fi

case "$verdict" in
  pass) state="success" ;;
  blocked) state="failure" ;;
  *) echo "$ME: bad verdict '$verdict' (want pass|blocked)" >&2; usage ;;
esac
[ -n "$note" ] || usage
[[ "$note" = /* ]] || refuse "--note must be an absolute path (got '$note')"
[ -f "$note" ] || refuse "verdict note not found: $note"
[ -z "$want_sha" ] || [[ "$want_sha" =~ ^[0-9a-fA-F]{7,40}$ ]] || refuse "bad --sha '$want_sha' (want 7 to 40 hex)"

sha="$(head_sha)"
short="${sha:0:7}"

if [ -n "$want_sha" ]; then
  want_sha="${want_sha,,}"
  [[ "$sha" = "$want_sha"* ]] ||
    refuse "stale review: reviewed $want_sha but PR #$pr head is now $short — review the new head"
fi

base="$(basename "$note")"
[[ "$base" = *-verdict.md ]] || refuse "not a verdict note (want *-verdict.md): $base"
IFS= read -r title < "$note" || true
[[ "$title" = "# Verdict"* ]] || refuse "not a verdict note (first line is not '# Verdict…'): $base"
grep -Eq "(#|pull/)$pr([^0-9]|\$)" "$note" || refuse "verdict note does not name PR #$pr: $base"
grep -Eiq "(^|[^0-9a-f])$short" "$note" ||
  refuse "verdict note does not name head $short — it reviewed another revision: $base"

description="$verdict — $base"
description="${description:0:140}"

gh_call api --method POST "repos/$repo/statuses/$sha" \
  -f "context=$CONTEXT" -f "state=$state" -f "description=$description" >/dev/null
echo "$CONTEXT: $state posted on PR #$pr head $short ($repo)"
