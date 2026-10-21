use super::{
    format_unknown_fence, ordinary_terminal_kickoff_attention, unknown_fence_detail,
    unknown_kickoff_attention, UNKNOWN_GENERIC_REASON,
};

const RENDER_MISS: &str = "submission accepted but never rendered on the endpoint";

#[test]
fn render_miss_reason_survives_restamp() {
    let once = format_unknown_fence(RENDER_MISS);
    let twice = format_unknown_fence(unknown_fence_detail(&once));
    assert_eq!(once, twice);
    assert!(once.starts_with(RENDER_MISS), "{once}");
    assert!(once.contains("does not prove"), "{once}");
    assert!(once.contains("--no-resume"), "{once}");
    assert!(!once.contains("then `cadence agent resume"), "{once}");
    assert!(!once.contains("job dispatch"), "{once}");
    assert!(!once.contains("cadence agent unfence"), "{once}");
    assert_ne!(
        unknown_fence_detail(&once),
        UNKNOWN_GENERIC_REASON,
        "{once}"
    );
}

#[test]
fn old_reconcile_marker_keeps_provider_account() {
    let old = "Connection lost during turn; provider outcome is unknown — reconcile: \
                   `cadence agent unfence w1 --status interrupted`, then \
                   `cadence agent resume w1`";
    let detail = unknown_fence_detail(old);
    assert_eq!(
        detail,
        "Connection lost during turn; provider outcome is unknown"
    );
    let restamped = format_unknown_fence(detail);
    assert!(
        restamped.contains("Connection lost during turn"),
        "{restamped}"
    );
    assert!(restamped.contains("does not prove"), "{restamped}");
    assert!(
        !restamped.contains("then `cadence agent resume"),
        "{restamped}"
    );
}

#[test]
fn empty_detail_stays_generic_without_a_command_chain() {
    let text = format_unknown_fence("  \n");
    assert!(text.starts_with(UNKNOWN_GENERIC_REASON), "{text}");
    assert!(text.contains(" — inspect:"), "{text}");
    assert!(!text.contains("then `cadence agent resume"), "{text}");
}

#[test]
fn detail_is_bounded_and_secret_shaped_argv_is_scrubbed() {
    let noisy = format!(
        "before\nsecret --api-key=supersecretvalue {}",
        "x".repeat(600)
    );
    let text = format_unknown_fence(&noisy);
    let detail = unknown_fence_detail(&text);
    assert!(!detail.contains('\n'), "{detail}");
    assert!(!detail.contains("supersecretvalue"), "{detail}");
    assert!(detail.contains("[REDACTED]"), "{detail}");
    assert!(detail.chars().count() <= 513, "{}", detail.chars().count());
    assert!(!text.contains("before_tail"), "{text}");
}

#[test]
fn unknown_kickoff_does_not_name_dispatch_ordinary_terminal_does() {
    let unknown = unknown_kickoff_attention("m1", "w1");
    assert!(unknown.contains("does not prove"), "{unknown}");
    assert!(unknown.contains("continuation is safe"), "{unknown}");
    assert!(!unknown.contains("job dispatch"), "{unknown}");
    assert!(!unknown.contains("cadence agent unfence"), "{unknown}");
    let failed = ordinary_terminal_kickoff_attention("m1", "failed", "task-a", 3);
    assert!(
        failed.contains("`cadence job dispatch task-a` starts revision 3"),
        "{failed}"
    );
    let interrupted = ordinary_terminal_kickoff_attention("m1", "interrupted", "task-a", 2);
    assert!(
        interrupted.contains("`cadence job dispatch task-a` starts revision 2"),
        "{interrupted}"
    );
}
