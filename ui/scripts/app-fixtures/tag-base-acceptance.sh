#!/usr/bin/env bash
set -euo pipefail

# Exercise the real Vite fixture guard for a first v* tag push whose before SHA
# is GitHub's all-zero sentinel. All source mutations stay in a private clone.
source_hint=${1:-$PWD}
source_repo=$(git -C "$source_hint" rev-parse --show-toplevel) || { echo 'SKIP: source repo unavailable'; exit 77; }
candidate=$(git -C "$source_repo" rev-parse HEAD) || { echo 'SKIP: candidate HEAD unavailable'; exit 77; }
if [[ $# -ge 2 ]]; then
  expected=$2
  [[ "$expected" =~ ^[0-9a-f]{40}$ && "$candidate" == "$expected" ]] || { echo "SKIP: candidate HEAD $candidate does not match requested full SHA"; exit 77; }
fi
[[ -z "$(git -C "$source_repo" status --porcelain=v1)" ]] || { echo 'SKIP: source worktree must be clean'; exit 77; }
for command in git node python3 sha256sum; do
  command -v "$command" >/dev/null || { echo "SKIP: required tool unavailable: $command"; exit 77; }
done
nodes="$source_repo/ui/node_modules"
[[ -x "$nodes/.bin/tsrs" && -d "$nodes/vite" ]] || { echo 'SKIP: existing UI TypeScript/Vite dependencies unavailable'; exit 77; }

root=$(mktemp -d "${TMPDIR:-/tmp}/cad1271-tag-base.XXXXXX")
mkdir -p "$root/logs" "$root/home" "$root/xdg-cache" "$root/xdg-config" "$root/tmp" "$root/bin"
exec > >(tee "$root/logs/run.log") 2>&1
real_git=$(command -v git)
cat >"$root/bin/git" <<'WRAPPER'
#!/usr/bin/env bash
# build-app-fixtures.mjs invokes git as: git -C <repo> fetch ... . Match
# that exact global-option form and log the untouched argv; delegate every
# other command (including cat-file, show, and remote inspection) unchanged.
if [[ $# -ge 3 && "$1" == -C && "$3" == fetch ]]; then
  printf 'git' >>"$CAD1271_FETCH_LOG"
  printf ' %q' "$@" >>"$CAD1271_FETCH_LOG"
  printf '\n' >>"$CAD1271_FETCH_LOG"
  echo 'test safety: network fetch denied' >&2
  exit 99
fi
exec "$CAD1271_REAL_GIT" "$@"
WRAPPER
chmod +x "$root/bin/git"
printf 'candidate=%s\nsource=%s\n' "$candidate" "$source_repo"

project="$root/project"
git clone --quiet --shared --no-checkout "$source_repo" "$project"
git -C "$project" checkout --quiet --detach "$candidate"
# CI validates the origin URL before fetching. Keep GitHub event metadata
# realistic while changing only this disposable clone's local remote config.
git -C "$project" remote set-url origin https://github.com/favcrm/cadence.git
ln -s "$nodes" "$project/ui/node_modules"
# CI checkout depth and PR merge parents vary. A local empty child commit makes
# the source candidate the guaranteed available parent without relying on them.
(cd "$project" && GIT_AUTHOR_NAME='Acceptance Fixture' GIT_AUTHOR_EMAIL='acceptance-fixture@invalid' GIT_COMMITTER_NAME='Acceptance Fixture' GIT_COMMITTER_EMAIL='acceptance-fixture@invalid' git commit --quiet --allow-empty -m 'local tag acceptance fixture')
test_commit=$(git -C "$project" rev-parse HEAD)
[[ "$(git -C "$project" rev-parse HEAD^1)" == "$candidate" ]] || { echo 'FAIL: private synthetic tag commit does not parent the source candidate'; exit 1; }
printf '{"before":"%s","after":"%s","ref":"refs/tags/v0.0.0-acceptance"}\n' "0000000000000000000000000000000000000000" "$test_commit" >"$root/push-event.json"
export CAD1271_FETCH_LOG="$root/logs/fetch-attempts.log"
export CAD1271_REAL_GIT="$real_git"
export PATH="$root/bin:$PATH"
export HOME="$root/home" XDG_CACHE_HOME="$root/xdg-cache" XDG_CONFIG_HOME="$root/xdg-config" TMPDIR="$root/tmp"
export CI=true GITHUB_EVENT_NAME=push GITHUB_EVENT_PATH="$root/push-event.json"
export GITHUB_REPOSITORY=favcrm/cadence GITHUB_SERVER_URL=https://github.com

# Real first-tag guard must use the current commit's parent as the trusted
# baseline and must not try to fetch the all-zero event.before value.
status=0
(cd "$project/ui" && node scripts/build-app-fixtures.mjs) >"$root/logs/zero-tag-positive.log" 2>&1 || status=$?
if [[ $status != 0 ]]; then
  echo "FAIL: zero-before tag push rejected by real fixture guard (exit=$status)"
  cat "$root/logs/zero-tag-positive.log"
  exit 1
fi
if [[ -s "$CAD1271_FETCH_LOG" ]]; then
  echo 'FAIL: real fixture guard attempted a network fetch for the zero before SHA'
  cat "$CAD1271_FETCH_LOG"
  exit 1
fi
printf 'PASS: zero-before tag push accepted using available trusted parent; real guard exit=%s; zero fetch attempts\n' "$status"

# Introduce a real canonical projection drift, regenerate the actual Vite
# producer and matching golden in the private clone, then ensure the same tag
# fallback still refuses that drift without a numeric contract bump.
python3 - "$project/ui/src/features/workspace-apps/screen/screenProjection.ts" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
s = p.read_text()
old = 'now: Math.floor(Date.now() / 1000),'
new = 'now: Math.floor(Date.now() / 1000) + 1,'
if s.count(old) != 1:
    raise SystemExit(f'expected one canonical projection expression; found {s.count(old)}')
p.write_text(s.replace(old, new))
PY
status=0
(cd "$project/ui" && env CI= GITHUB_EVENT_NAME= GITHUB_EVENT_PATH= node scripts/build-app-fixtures.mjs --write) >"$root/logs/mutated-write.log" 2>&1 || status=$?
if [[ $status != 0 ]]; then
  echo "FAIL: real Vite producer could not regenerate mutated artifact and golden (exit=$status)"
  cat "$root/logs/mutated-write.log"
  exit 1
fi
: >"$CAD1271_FETCH_LOG"
status=0
(cd "$project/ui" && node scripts/build-app-fixtures.mjs) >"$root/logs/zero-tag-drift-refusal.log" 2>&1 || status=$?
if [[ $status == 0 ]]; then
  echo 'REVISE: zero-before tag fallback accepted canonical projection drift at the unchanged numeric contract'
  exit 3
fi
if ! grep -Fq 'without a numeric contract bump' "$root/logs/zero-tag-drift-refusal.log"; then
  echo "REVISE: unexpected refusal (exit=$status), not the numeric-contract guard"
  cat "$root/logs/zero-tag-drift-refusal.log"
  exit 3
fi
if grep -Eqi 'error TS[0-9]+|cannot find module|not found|merge-base|trusted base.*unavailable|network fetch denied|fatal:' "$root/logs/zero-tag-drift-refusal.log"; then
  echo 'REVISE: refusal was caused by toolchain, Git, or fetch failure rather than the numeric-contract guard'
  cat "$root/logs/zero-tag-drift-refusal.log"
  exit 3
fi
if [[ -s "$CAD1271_FETCH_LOG" ]]; then
  echo 'REVISE: drift check attempted a fetch instead of using the trusted parent already available locally'
  cat "$CAD1271_FETCH_LOG"
  exit 3
fi
printf 'PASS: real guard refused regenerated canonical projection drift at unchanged contract (exit=%s)\n' "$status"
cat "$root/logs/zero-tag-drift-refusal.log"
echo "retained_logs=$root/logs"
