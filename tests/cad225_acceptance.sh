#!/bin/sh
# CAD-225 bounded acceptance harness.
#
# This is a fixture-only gate. It drives existing temporary-state tests and
# records the capabilities that are deliberately outside the current public
# contract. It never starts a live daemon, uses credentials, calls GitHub, or
# performs a merge. Exit 0 means every supported case passed; exit 2 means
# the supported cases passed but the current product still has explicit
# acceptance gaps; exit 1 means a supported case failed.

set -u

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
LOG_ROOT=${CAD225_LOG_ROOT:-${TMPDIR:-/tmp}/cad225-acceptance-$$}
mkdir -p "$LOG_ROOT"

passed=0
failed=0
unsupported=0
failed_labels=

run_case() {
    label=$1
    shift
    log="$LOG_ROOT/$label.log"
    printf 'CASE %s\n' "$label"
    printf '  command:'
    printf ' %s' "$@"
    printf '\n'
    if "$@" >"$log" 2>&1; then
        passed=$((passed + 1))
        printf '  result: PASS (log %s)\n' "$log"
    else
        rc=$?
        failed=$((failed + 1))
        failed_labels="$failed_labels $label"
        printf '  result: FAIL exit=%s (log %s)\n' "$rc" "$log"
        tail -30 "$log" >&2 || true
    fi
}

run_cargo_case() {
    label=$1
    target=$2
    filter=$3
    shift 3
    case "$target" in
        integration)
            run_case "$label" env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
                cargo test --locked --test integration "$filter" -- --exact --nocapture "$@"
            ;;
        board)
            run_case "$label" env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
                cargo test --locked --test board "$filter" -- --exact --nocapture "$@"
            ;;
        lib)
            run_case "$label" env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
                cargo test --locked --lib "$filter" -- --exact --nocapture "$@"
            ;;
        *)
            printf 'unknown test target %s\n' "$target" >&2
            failed=$((failed + 1))
            failed_labels="$failed_labels $label"
            ;;
    esac
}

cd "$ROOT" || exit 1
head_commit=$(git rev-parse HEAD)
tree_sha=$(git rev-parse HEAD^{tree})

# Goal/plan/task creation and the complete existing job state machine:
# dispatch, durable completion, independent review, revision, verified edge
# and the merge-evidence acceptance edge.
run_cargo_case goal_plan_task integration issue_start_job_opens_scoped_task
run_cargo_case job_review_revision_accept integration job_seeded_bug_loop_end_to_end

# Exactly-once and restart/lost-wake protections in the current store.
run_cargo_case duplicate_dispatch integration dispatch_dedupes_live_kickoff_and_reassign_bumps
run_cargo_case restart_lost_wake integration restart_fences_task_kickoff_and_job_show_reports_drift
run_cargo_case provider_approval integration approval_lifecycle
run_cargo_case missing_recipient_identity lib store::tests::missing_job_event_recipient_is_durable_and_not_replayed
run_cargo_case identity_change lib store::tests::finish_refuses_re_registered_alias_with_changed_identity

# Current watchdog boundary: durable observation plus explicit guarded handoff,
# with alert deduplication and restart-safe monitor state.
run_cargo_case monitor_restart_alert_dedupe integration monitor_persists_coverage_heartbeats_and_deduplicates_alerts
run_cargo_case monitor_guarded_dispatch integration monitor_dispatch_requires_explicit_safe_eligibility

# Exact-head and author/reviewer separation. This is a store-level check so it
# does not depend on a live provider or a GitHub identity.
run_cargo_case stale_head_and_self_review lib store::tests::verdict_revision_and_reviewer_guards

# The issue retro and memory APIs are read/write gated, but lesson promotion is
# curator-driven. Keep the fixture check separate from the unsupported claims.
run_cargo_case lesson_artifact board memory_round_trip_and_trailers
run_cargo_case retro_preview board retro_reports_rounds_defects_flakes_and_unknowns

printf '\nUNSUPPORTED CAD225 acceptance steps on current main:\n'
unsupported=$((unsupported + 1))
printf '  - unattended opt-in scheduler: monitor watch records alerts only; it never calls monitor_dispatch\n'
unsupported=$((unsupported + 1))
printf '  - provider quota exhaustion admission/recovery: no job/monitor quota contract exists; relay quota is separate\n'
unsupported=$((unsupported + 1))
printf '  - policy-authorized merge: job accept records merged_sha evidence; Cadence does not execute or authorize git/GitHub merge\n'
unsupported=$((unsupported + 1))
printf '  - moved PR head between review and merge: local task SHA guards pass, but no remote head watch or merge binding exists in this fixture\n'
unsupported=$((unsupported + 1))
printf '  - exactly one actionable UI escalation per unresolved cause: local alert dedupe exists, delivery is unconfigured and no coordinator escalation state exists\n'
unsupported=$((unsupported + 1))
printf '  - real-provider smoke and actual deployed-version proof: intentionally omitted; no live credentials, daemon activation, or deployment is allowed in this fixture gate\n'
unsupported=$((unsupported + 1))
printf '  - independently reviewed lesson promotion and later dispatch injection: retro is a preview and memory acceptance remains a separate curator action\n'

result=PASS
exit_code=0
if [ "$failed" -gt 0 ]; then
    result=FAIL
    exit_code=1
elif [ "$unsupported" -gt 0 ]; then
    result=BLOCKED
    exit_code=2
fi

cat >"$LOG_ROOT/summary.txt" <<EOF
CAD225 result: $result
head_commit: $head_commit
tree: $tree_sha
supported_passed: $passed
supported_failed: $failed
unsupported_steps: $unsupported
logs: $LOG_ROOT
failed_labels:${failed_labels:- none}
EOF
cat "$LOG_ROOT/summary.txt"
exit "$exit_code"
