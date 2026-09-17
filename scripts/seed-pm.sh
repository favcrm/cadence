#!/usr/bin/env bash
# Seed the PM dir (~/pm or CADENCE_PM_DIR) with the iteration-1 board data.
# Every write goes through `cadence issue` — one git commit each — so the
# seeded history is exactly what the CLI would have produced by hand.
#
# Usage:  scripts/seed-pm.sh [pm-dir]
# Env:    CADENCE   path to the cadence binary (default: target/release/cadence
#                   if built, else `cadence` on PATH)
#         NOTES     agent-notes directory (default: /var/www/agent-notes)
#
# Refuses to run against an existing pm.yaml — seeding is a one-time act.

set -euo pipefail

PM_DIR="${1:-${CADENCE_PM_DIR:-$HOME/pm}}"
NOTES="${NOTES_DIR:-/var/www/agent-notes}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [[ -n "${CADENCE:-}" ]]; then
  BIN="$CADENCE"
elif [[ -x "$ROOT/target/release/cadence" ]]; then
  BIN="$ROOT/target/release/cadence"
else
  BIN="$(command -v cadence)"
fi

if [[ -f "$PM_DIR/pm.yaml" ]]; then
  echo "seed: $PM_DIR/pm.yaml already exists — refusing to reseed." >&2
  echo "      point CADENCE_PM_DIR at a fresh dir for a test seed." >&2
  exit 1
fi

export CADENCE_PM_DIR="$PM_DIR"
note() { [[ -f "$NOTES/$1" ]] && printf '%s' "$NOTES/$1" || printf '%s' "$1"; }
say() { printf 'seed: %s\n' "$*" >&2; }

say "pm dir: $PM_DIR (binary: $BIN)"

"$BIN" issue init

"$BIN" issue project add cadence --prefix CAD \
  --repo "$HOME/Project/cadence" \
  --component adapter --component daemon --component cli --component ui \
  --owner cookie-cesium
"$BIN" issue project add sportslog --prefix SPL \
  --repo "$HOME/Project/sportslog" \
  --component app --component api --component admin --component web
"$BIN" issue project add ops --prefix OPS --owner operator

# ---- cadence: done history first (link targets), then the backlog ----
"$BIN" issue new "Independent QA audit of main @ e663778" --id CAD-9 --priority P1 --owner qa-audit-319f2
"$BIN" issue new "Inbox endpoint + verified auto-ready for pty" --id CAD-12 --priority P0 --blocked-by CAD-9 --owner cookie-cesium
"$BIN" issue new "Fence recovery: reconcile, unfence, restart skips fenced" --id CAD-14 --priority P0 --blocked-by CAD-12 --owner cookie-cesium
"$BIN" issue new "claude provider" --id CAD-30 --priority P1
"$BIN" issue new "M3 jobs and tasks design" --id CAD-11 --priority P1 --owner devin-056801
"$BIN" issue new "claude provider: managed stream-json endpoint" --id CAD-16 --priority P1 \
  --parent CAD-30 --component adapter --blocked-by CAD-12 --blocked-by CAD-14 \
  --owner cookie-cesium
"$BIN" issue new "claude provider: pty TUI endpoint with Stop-hook reporting" --id CAD-17 \
  --priority P1 --parent CAD-30 --blocked-by CAD-16
"$BIN" issue new "devin --bypass, or an allowlist written into the worktree" --id CAD-18 --priority P1
"$BIN" issue new "Broker approvals via the permission prompt tool" --id CAD-19 \
  --priority P2 --parent CAD-30 --blocked-by CAD-16
"$BIN" issue new "Launch must not write into the cwd repo (N8)" --id CAD-20 --priority P1
"$BIN" issue new "Settle agent unfence default: resume or stay stopped" --id CAD-21 \
  --priority P2 --blocked-by CAD-14
"$BIN" issue new "Internal board: issue folders + cadence ui" --id CAD-22 --priority P2 --owner operator
"$BIN" issue new "message result text truncated on long single lines" --id CAD-23 --priority P2
"$BIN" issue new "dead flag is misleading for non-pty kinds (N5)" --id CAD-24 --priority P3
"$BIN" issue new "Cancel or requeue a queued message (G4)" --id CAD-25 --priority P3
"$BIN" issue new "M3a: jobs, tasks and verdicts in the daemon" --id CAD-26 --priority P1 \
  --blocked-by CAD-16

# relates links (not expressible at `new`)
"$BIN" issue link CAD-16 relates CAD-18
"$BIN" issue link CAD-26 relates CAD-22

# ---- sportslog ----
"$BIN" issue new "Friend add lands in Friends, leaves Matching, keeps the room" \
  --project sportslog --id SPL-4 --priority P1 --owner devin --component app
"$BIN" issue new "Re-match path for friends whose match expired" --project sportslog --id SPL-5 \
  --priority P2 --component app --blocked-by SPL-4
"$BIN" issue new "Prod D1 migrations 0019 and 0020" --project sportslog --id SPL-2 --priority P2 --component api

# ---- ops ----
"$BIN" issue new "Gateway vhost for cadence.localhost" --project ops --id OPS-3 --priority P2
"$BIN" issue link OPS-3 relates CAD-22

# ---- statuses (file source; notes/M3 derivation overrides when present) ----
"$BIN" issue set CAD-9  status=done
"$BIN" issue set CAD-12 status=done
"$BIN" issue set CAD-14 status=done
"$BIN" issue set CAD-30 status=doing
"$BIN" issue set CAD-11 status=review
"$BIN" issue set CAD-16 status=doing
"$BIN" issue set CAD-22 status=doing
"$BIN" issue set CAD-18 status=ready
"$BIN" issue set CAD-20 status=ready
"$BIN" issue set CAD-21 status=ready
# CAD-26 stays backlog: blocked_by CAD-16 (doing) — ready+open-blocker
# was a seed mistake; lint now warns on it.
"$BIN" issue set SPL-2 status=ready
"$BIN" issue set SPL-4 status=review
"$BIN" issue set OPS-3 status=ready

# ---- refs ----
"$BIN" issue ref CAD-16 pr https://github.com/favcrm/cadence/pull/16 --label "PR #16"
"$BIN" issue ref CAD-16 note "$(note 20260917-160143-319f2-cadence-claude-managed-kickoff.md)" --label "kickoff · claude-managed"
"$BIN" issue ref CAD-16 note "$(note 20260917-172603-319f2-cadence-claude-managed-review-kickoff.md)" --label "review kickoff · PR 16"
"$BIN" issue ref CAD-16 note "$(note 20260917-180927-319f2-cadence-claude-managed-verdict.md)" --label "verdict"
"$BIN" issue ref CAD-14 pr https://github.com/favcrm/cadence/pull/15 --label "PR #15"
"$BIN" issue ref CAD-14 commit cb3e9cc
"$BIN" issue ref CAD-12 pr https://github.com/favcrm/cadence/pull/14 --label "PR #14"
"$BIN" issue ref CAD-12 commit 804c821
"$BIN" issue ref CAD-22 preview /20260917-164658/cadence-board-plan/ --label "plan + mock"
"$BIN" issue ref CAD-22 note "$(note 20260917-174323-60747-cadence-board-i1-kickoff.md)" --label "kickoff"

# ---- comments ----
"$BIN" issue comment CAD-16 --author cookie-cesium -m \
  "Observed on the real CLI: \`system/init\` only arrives with the first turn, so the session check moved into \`run_turn\`."
"$BIN" issue comment CAD-16 --author fable-cc -m \
  "Review: the 600 s turn deadline is fixed. A real implementation turn runs far longer, so this fences healthy work. Details in the review kickoff."
"$BIN" issue comment CAD-16 --author operator -m \
  "Agreed. Activity-based liveness, default 15 min idle."
"$BIN" issue comment CAD-17 --author operator -m "Wait on CAD-16; the pty kind reuses its session probe."
"$BIN" issue comment CAD-22 --author operator -m "Mock v4 is the spec — see the preview ref."
"$BIN" issue comment CAD-26 --author cookie-cesium -m "Design lives under CAD-11; M3a is the daemon half."
"$BIN" issue comment CAD-14 --author cookie-cesium -m "Reconcile runs on daemon start; fenced agents are skipped, not probed."
"$BIN" issue comment CAD-14 --author operator -m "Verified on the live daemon — restart kept fences."
"$BIN" issue comment CAD-12 --author cookie-cesium -m "Inbox kind added; pty claims verified by inbox render, not by echo."
"$BIN" issue comment CAD-12 --author qa-audit-319f2 -m "Probe regression found and fixed in the same loop."
"$BIN" issue comment CAD-12 --author operator -m "Auto-ready holds across daemon restart."
"$BIN" issue comment CAD-12 --author cookie-cesium -m "Landed as 804c821."

# ---- artifacts ----
ART="$(mktemp -d)"
trap 'rm -rf "$ART"' EXIT
head -c 1200 /dev/urandom | base64 > "$ART/turn1.jsonl"
head -c 2900 /dev/urandom | base64 > "$ART/denied.jsonl"
head -c 4100 /dev/urandom | base64 > "$ART/scratch-smoke.txt"
"$BIN" issue attach CAD-16 "$ART/turn1.jsonl"
"$BIN" issue attach CAD-16 "$ART/denied.jsonl"
"$BIN" issue attach CAD-16 "$ART/scratch-smoke.txt"
"$BIN" issue attach CAD-22 "$ROOT/ui/design/board-mock-v4.html"
printf '# cadence M3: jobs, tasks and verdicts\n\nDesign draft — see the CAD-11 notes chain.\n' > "$ART/cadence-JOBS.md"
"$BIN" issue attach CAD-11 "$ART/cadence-JOBS.md"

"$BIN" issue lint
say "done — $(cd "$PM_DIR" && git log --oneline | wc -l) commits in $PM_DIR"
