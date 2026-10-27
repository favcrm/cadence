#!/usr/bin/env bash
set -euo pipefail

# Usage: drift-acceptance.sh [source-repo [expected-full-head]]
# Runs the actual local host fixture gate in private clones; no product files are edited.
source_hint=${1:-$PWD}
source_repo=$(git -C "$source_hint" rev-parse --show-toplevel) || { echo 'SKIP: source repo not available'; exit 77; }
candidate=$(git -C "$source_repo" rev-parse HEAD) || { echo 'SKIP: candidate HEAD unavailable'; exit 77; }
if [[ $# -ge 2 ]]; then
  expected=$2
  [[ "$expected" =~ ^[0-9a-f]{40}$ ]] || { echo 'SKIP: expected head must be a full lowercase SHA'; exit 77; }
  [[ "$candidate" == "$expected" ]] || { echo "SKIP: HEAD $candidate differs from requested $expected"; exit 77; }
fi
[[ -z "$(git -C "$source_repo" status --porcelain=v1)" ]] || { echo 'SKIP: source worktree must be clean'; exit 77; }

for command in git node python3 sha256sum; do
  command -v "$command" >/dev/null || { echo "SKIP: required tool unavailable: $command"; exit 77; }
done
nodes="$source_repo/ui/node_modules"
[[ -x "$nodes/.bin/tsrs" && -d "$nodes/vite" ]] || { echo 'SKIP: existing UI TypeScript/Vite dependencies unavailable'; exit 77; }

artifact_path=src/app_preview_export/producer.mjs
baseline_path=src/app_preview_export/baseline.json
artifact_sha=$(git -C "$source_repo" show "$candidate:$artifact_path" | sha256sum | cut -d' ' -f1)
baseline_sha=$(git -C "$source_repo" show "$candidate:$baseline_path" | sha256sum | cut -d' ' -f1)
[[ "$(sha256sum "$source_repo/$artifact_path" | cut -d' ' -f1)" == "$artifact_sha" ]] || { echo 'SKIP: worktree artifact differs from committed candidate'; exit 77; }
[[ "$(sha256sum "$source_repo/$baseline_path" | cut -d' ' -f1)" == "$baseline_sha" ]] || { echo 'SKIP: worktree baseline differs from committed candidate'; exit 77; }
contract=$(git -C "$source_repo" show "$candidate:$baseline_path" | node -e 'let s="";process.stdin.on("data",x=>s+=x).on("end",()=>process.stdout.write(JSON.parse(s).contract))')
[[ "$contract" =~ ^app-screen-projection-fixtures/[0-9]+$ ]] || { echo 'SKIP: committed baseline contract is not numeric'; exit 77; }

root=$(mktemp -d "${TMPDIR:-/tmp}/cad1271-drift.XXXXXX")
mkdir -p "$root/logs" "$root/home" "$root/xdg-cache" "$root/xdg-config" "$root/tmp"
exec > >(tee "$root/logs/run.log") 2>&1
printf 'candidate=%s\nartifact_sha256=%s\nbaseline_file_sha256=%s\ncontract=%s\nsource=%s\n' "$candidate" "$artifact_sha" "$baseline_sha" "$contract" "$source_repo"

make_copy() {
  local path=$1
  git clone --quiet --shared --no-checkout "$source_repo" "$path"
  git -C "$path" checkout --quiet --detach "$candidate"
  # The real local guard chooses merge-base(HEAD, origin/main); keep the trust
  # reference private to each copy and pinned to this exact frozen candidate.
  git -C "$path" update-ref refs/remotes/origin/main "$candidate"
  [[ "$(git -C "$path" rev-parse HEAD)" == "$candidate" ]]
  [[ "$(git -C "$path" merge-base HEAD refs/remotes/origin/main)" == "$candidate" ]]
  ln -s "$nodes" "$path/ui/node_modules"
}
run_guard() {
  local project=$1 log=$2 mode=$3 status=0
  if [[ "$mode" == write ]]; then
    (cd "$project/ui" && env HOME="$root/home" XDG_CACHE_HOME="$root/xdg-cache" XDG_CONFIG_HOME="$root/xdg-config" TMPDIR="$root/tmp" CI= GITHUB_EVENT_PATH= node scripts/build-app-fixtures.mjs --write) >"$log" 2>&1 || status=$?
  else
    (cd "$project/ui" && env HOME="$root/home" XDG_CACHE_HOME="$root/xdg-cache" XDG_CONFIG_HOME="$root/xdg-config" TMPDIR="$root/tmp" CI= GITHUB_EVENT_PATH= node scripts/build-app-fixtures.mjs) >"$log" 2>&1 || status=$?
  fi
  printf '%s\n' "$status"
}

# Independent relocations prove regeneration is rooted in canonical repo paths.
make_copy "$root/location-a/project"
make_copy "$root/location-b/project"
for location in location-a location-b; do
  project="$root/$location/project"
  status=$(run_guard "$project" "$root/logs/$location-write.log" write)
  [[ "$status" == 0 ]] || { echo "FAIL: $location --write exit=$status"; cat "$root/logs/$location-write.log"; exit 1; }
  got_artifact=$(sha256sum "$project/$artifact_path" | cut -d' ' -f1)
  got_baseline=$(sha256sum "$project/$baseline_path" | cut -d' ' -f1)
  [[ "$got_artifact" == "$artifact_sha" && "$got_baseline" == "$baseline_sha" ]] || { echo "FAIL: $location regeneration differs from frozen committed hashes"; exit 1; }
  echo "$location --write exit=0 artifact=$got_artifact baseline=$got_baseline"
done

project="$root/location-a/project"
status=$(run_guard "$project" "$root/logs/positive.log" check)
[[ "$status" == 0 ]] || { echo "FAIL: positive real guard exit=$status"; cat "$root/logs/positive.log"; exit 1; }
echo 'positive real guard exit=0'

# Alter only canonical screen projection output, then regenerate checked artifact
# and golden; the contract stays at the committed version.
projection="$project/ui/src/features/workspace-apps/screen/screenProjection.ts"
python3 - "$projection" <<'PY'
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
status=$(run_guard "$project" "$root/logs/mutated-write.log" write)
[[ "$status" == 0 ]] || { echo "FAIL: mutated --write exit=$status"; cat "$root/logs/mutated-write.log"; exit 1; }
mutated_artifact=$(sha256sum "$project/$artifact_path" | cut -d' ' -f1)
mutated_baseline=$(sha256sum "$project/$baseline_path" | cut -d' ' -f1)
[[ "$mutated_artifact" != "$artifact_sha" && "$mutated_baseline" != "$baseline_sha" ]] || { echo 'FAIL: regenerated artifact or baseline did not change'; exit 1; }
mutated_contract=$(node -e 'process.stdout.write(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).contract)' "$project/$baseline_path")
mutated_golden=$(node -e 'process.stdout.write(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).golden_sha256)' "$project/$baseline_path")
committed_golden=$(git -C "$project" show "$candidate:$baseline_path" | node -e 'let s="";process.stdin.on("data",x=>s+=x).on("end",()=>process.stdout.write(JSON.parse(s).golden_sha256))')
[[ "$mutated_contract" == "$contract" ]] || { echo "FAIL: mutation changed contract: $mutated_contract"; exit 1; }
[[ "$mutated_golden" != "$committed_golden" ]] || { echo 'FAIL: canonical wire golden did not change'; exit 1; }
echo "mutated --write exit=0 artifact=$mutated_artifact baseline=$mutated_baseline contract=$mutated_contract golden=$mutated_golden"

status=$(run_guard "$project" "$root/logs/refusal.log" check)
[[ "$status" != 0 ]] || { echo 'REVISE: real guard accepted projection drift without a contract bump'; exit 3; }
grep -Fq 'without a numeric contract bump' "$root/logs/refusal.log" || { echo "REVISE: unexpected refusal exit=$status"; cat "$root/logs/refusal.log"; exit 3; }
grep -Eqi 'error TS[0-9]+|cannot find module|not found|merge-base|trusted base.*unavailable|fatal:' "$root/logs/refusal.log" && { echo 'REVISE: refusal included a toolchain/Git failure'; cat "$root/logs/refusal.log"; exit 3; }
echo "PASS: real projection drift gate refused unchanged contract (guard exit=$status)"
cat "$root/logs/refusal.log"
echo "PASS candidate=$candidate committed_artifact=$artifact_sha committed_baseline=$baseline_sha mutated_artifact=$mutated_artifact mutated_baseline=$mutated_baseline committed_golden=$committed_golden mutated_golden=$mutated_golden"
echo "retained_logs=$root/logs"
