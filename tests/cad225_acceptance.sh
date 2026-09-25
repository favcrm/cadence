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
NEXTEST_BIN=${CADENCE_NEXTTEST_BIN:-cargo-nextest}
SUITE_LOCK=${CADENCE_SUITE_LOCK:-}
mkdir -p "$LOG_ROOT"

passed=0
failed=0
unsupported=0
harness_passed=0
harness_failed=0
failed_labels=

write_summary() {
    cat >"$LOG_ROOT/summary.txt" <<EOF
CAD225 result: $result
head_commit: $head_commit
tree: $tree_sha
tree_clean_before: $tree_clean_before
tree_clean_after: $tree_clean_after
head_unchanged: $head_unchanged
supported_passed: $passed
supported_failed: $failed
harness_checks_passed: $harness_passed
harness_checks_failed: $harness_failed
unsupported_steps: $unsupported
logs: $LOG_ROOT
failed_labels:${failed_labels:- none}
EOF
    cat "$LOG_ROOT/summary.txt"
}

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
        lib)
            run_case "$label" env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
                CADENCE_NEXTTEST_BIN="$NEXTEST_BIN" CADENCE_SUITE_LOCK="$SUITE_LOCK" \
                "$ROOT/scripts/cadence-nextest" --lib --locked \
                -E "test(=$filter)" "$@"
            ;;
        *)
            # CAD-426: any other value is a tests/<target>.rs binary stem —
            # the old monolith was split into per-area binaries.
            run_case "$label" env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
                CADENCE_NEXTTEST_BIN="$NEXTEST_BIN" CADENCE_SUITE_LOCK="$SUITE_LOCK" \
                "$ROOT/scripts/cadence-nextest" --test "$target" --locked \
                -E "test(=$filter)" "$@"
            ;;
    esac
}

run_expected_missing_filter() {
    label=$1
    target=$2
    filter=$3
    log="$LOG_ROOT/$label.log"
    case "$target" in
        lib) target_args="--lib --locked" ;;
        *) target_args="--test $target --locked" ;;
    esac
    printf 'HARNESS %s\n' "$label"
    printf '  command: env CARGO_BUILD_JOBS=%s CADENCE_NEXTTEST_BIN=%s CADENCE_SUITE_LOCK=%s %s %s -E test(=%s)\n' \
        "${CARGO_BUILD_JOBS:-4}" "$NEXTEST_BIN" "$SUITE_LOCK" \
        "$ROOT/scripts/cadence-nextest" "$target_args" "$filter"
    if env CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
        CADENCE_NEXTTEST_BIN="$NEXTEST_BIN" CADENCE_SUITE_LOCK="$SUITE_LOCK" \
        "$ROOT/scripts/cadence-nextest" $target_args -E "test(=$filter)" \
        >"$log" 2>&1; then
        harness_failed=$((harness_failed + 1))
        failed_labels="$failed_labels $label"
        printf '  result: FAIL (missing filter was accepted; log %s)\n' "$log"
        tail -30 "$log" >&2 || true
    elif grep -Eiq 'no tests to run|0 tests run' "$log"; then
        harness_passed=$((harness_passed + 1))
        printf '  result: PASS (pinned runner rejected empty selection; log %s)\n' "$log"
    else
        harness_failed=$((harness_failed + 1))
        failed_labels="$failed_labels $label"
        printf '  result: FAIL (runner failed for an unrelated reason; log %s)\n' "$log"
        tail -30 "$log" >&2 || true
    fi
}

cd "$ROOT" || exit 1
head_commit=$(git rev-parse HEAD)
tree_sha=$(git rev-parse HEAD^{tree})
initial_status=$(git status --porcelain --untracked-files=all)
tree_clean_before=yes
tree_clean_after=unknown
head_unchanged=unknown
if [ -n "$initial_status" ]; then
    tree_clean_before=no
    result=FAIL
    failed=1
    failed_labels=" initial_dirty_tree"
    printf 'INITIAL TREE DIRTY; refusing to claim a clean tested tree:\n%s\n' "$initial_status" >&2
    tree_clean_after=no
    head_unchanged=yes
    write_summary
    exit 1
fi

# The shared runner intentionally has no lock-path default.  Requiring the
# caller to provide it prevents this fixture harness from silently bypassing
# the host-wide suite admission contract.  CADENCE_NEXTTEST_BIN still follows
# the runner's portable cargo-nextest default above.
if [ -z "$SUITE_LOCK" ]; then
    result=FAIL
    failed=1
    failed_labels=" missing_suite_lock"
    printf 'CADENCE_SUITE_LOCK is required; set the canonical host lock or use cadence review\n' >&2
    tree_clean_after=yes
    head_unchanged=yes
    write_summary
    exit 1
fi
mkdir -p "$(dirname -- "$SUITE_LOCK")"

# The wrapper supplies --no-tests fail and a pinned binary/checksum. This
# expected-failure probe proves a renamed or missing filter cannot silently
# turn into a green case with zero executed tests.
run_expected_missing_filter missing_filter_rejected daemon cad225_missing_filter_probe

# Goal/plan/task creation and the complete existing job state machine:
# dispatch, durable completion, independent review, revision, verified edge
# and the merge-evidence acceptance edge.
run_cargo_case goal_plan_task tracker_issue issue_start_job_opens_scoped_task
run_cargo_case job_review_revision_accept dispatch_jobs_monitor job_seeded_bug_loop_end_to_end

# Exactly-once and restart/lost-wake protections in the current store.
run_cargo_case duplicate_dispatch dispatch_jobs_monitor dispatch_dedupes_live_kickoff_and_reassign_bumps
run_cargo_case restart_lost_wake daemon restart_fences_task_kickoff_and_job_show_reports_drift
run_cargo_case provider_approval daemon approval_lifecycle
run_cargo_case missing_recipient_identity lib store::tests::missing_job_event_recipient_is_durable_and_not_replayed
run_cargo_case identity_change lib store::tests::finish_refuses_re_registered_alias_with_changed_identity

# Current watchdog boundary: durable observation plus explicit guarded handoff,
# with alert deduplication and restart-safe monitor state.
run_cargo_case monitor_restart_alert_dedupe monitor monitor_persists_coverage_heartbeats_and_deduplicates_alerts
run_cargo_case monitor_guarded_dispatch monitor monitor_dispatch_requires_explicit_safe_eligibility

# Exact-head and author/reviewer separation. This is a store-level check so it
# does not depend on a live provider or a GitHub identity.
run_cargo_case stale_head_and_self_review lib store::tests::verdict_revision_and_reviewer_guards

# The issue retro and memory APIs are read/write gated, but lesson promotion is
# curator-driven. Keep the fixture check separate from the unsupported claims.
run_cargo_case lesson_artifact board memory_round_trip_and_trailers
run_cargo_case retro_preview board retro_reports_rounds_defects_flakes_and_unknowns

printf '\nUNSUPPORTED CAD225 acceptance steps on current main:\n'
unsupported=$((unsupported + 1))
printf '  - unattended opt-in scheduler end-to-end: CAD-176 PR100 owns the coordinator; this fixture does not exercise that pending coordinator or its post-merge behavior\n'
unsupported=$((unsupported + 1))
printf '  - provider quota exhaustion admission/recovery: PR100 requires fresh provider-tagged evidence, but no real provider producer/current-production signal exists; live quota recovery is outside this fixture\n'
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

post_status=$(git status --porcelain --untracked-files=all)
post_head=$(git rev-parse HEAD)
post_tree=$(git rev-parse HEAD^{tree})
if [ -n "$post_status" ]; then
    tree_clean_after=no
else
    tree_clean_after=yes
fi
if [ "$post_head" = "$head_commit" ] && [ "$post_tree" = "$tree_sha" ]; then
    head_unchanged=yes
else
    head_unchanged=no
fi
if [ "$tree_clean_after" != yes ] || [ "$head_unchanged" != yes ]; then
    failed=$((failed + 1))
    failed_labels="$failed_labels tested_tree_changed"
    printf 'TESTED TREE CHANGED; refusing to claim the recorded head/tree:\n' >&2
    printf '  start: %s %s\n' "$head_commit" "$tree_sha" >&2
    printf '  end:   %s %s\n' "$post_head" "$post_tree" >&2
    printf '%s\n' "$post_status" >&2
fi

result=PASS
exit_code=0
if [ "$failed" -gt 0 ] || [ "$harness_failed" -gt 0 ]; then
    result=FAIL
    exit_code=1
elif [ "$unsupported" -gt 0 ]; then
    result=BLOCKED
    exit_code=2
fi

write_summary
exit "$exit_code"
