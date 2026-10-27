//! CAD-1159 N1: native-qualification diagnostics for S-VIEW-1. Whole module
//! exists ONLY under `cfg(all(debug_assertions, feature = "test-seam"))`
//! (declared so in `pi_guest/mod.rs`); the crate's `compile_error!` in
//! `test_seam.rs` already rejects that feature in any release build. No env
//! var, runtime flag or default-feature path reaches it.
//!
//! 1. Refusal-only seam: [`before_fchmod`] is called by
//!    `GenerationView::isolate` after every proof/correspondence check and
//!    immediately before `fchmod`. When armed for one exact launch
//!    [`Selection`] it returns an error once, records a hit, and disarms.
//!    It has no success path of its own: `Ok(())` only means "production
//!    continues unchanged".
//! 2. Pre-teardown observation: [`HeldObservation`] is a read-only snapshot of
//!    whether the retained RunningLaunch and GenerationView are Unknown.
use crate::error::{Error, Result};
use crate::protected_pi_profile::authority::Selection;
use std::sync::Mutex;
use std::time::SystemTime;

/// Unique marker for release-binary absence checks (`strings`/`nm`).
pub(crate) const REFUSAL_MARKER: &str = "cadence-n1-s-view-1-isolate-seam-refusal-v1";

struct State {
    armed: Vec<Selection>,
    hits: Vec<Selection>,
}
static STATE: Mutex<State> = Mutex::new(State {
    armed: Vec::new(),
    hits: Vec::new(),
});
fn state() -> std::sync::MutexGuard<'static, State> {
    STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Arm for exactly one held launch (its stable `Selection`: alias hash +
/// generation + role + model). Re-arming an already armed launch is refused.
#[allow(dead_code)] // consumed by the native harness and the tests below
pub(crate) fn arm(launch: &Selection) -> Result<()> {
    let mut state = state();
    if state.armed.contains(launch) {
        return Err(Error::rejected(
            "isolate seam already armed for this launch",
        ));
    }
    state.armed.push(launch.clone());
    Ok(())
}

/// Remove an unfired arming. Returns whether one was armed.
#[allow(dead_code)]
pub(crate) fn disarm(launch: &Selection) -> bool {
    let mut state = state();
    let before = state.armed.len();
    state.armed.retain(|armed| armed != launch);
    state.armed.len() != before
}

/// Launches whose armed refusal has fired, in order.
#[allow(dead_code)]
pub(crate) fn hits() -> Vec<Selection> {
    state().hits.clone()
}

/// Called ONLY from `GenerationView::isolate`, just before `fchmod`.
/// Fires at most once per arming, for the armed launch only.
pub(super) fn before_fchmod(launch: &Selection) -> Result<()> {
    let mut state = state();
    let Some(index) = state.armed.iter().position(|armed| armed == launch) else {
        return Ok(());
    };
    state.armed.remove(index);
    state.hits.push(launch.clone());
    Err(Error::rejected(format!(
        "{REFUSAL_MARKER}: armed one-shot refusal before fchmod"
    )))
}

/// Read-only pre-teardown snapshot. `at` is wall-clock at observation.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct HeldObservation {
    pub(crate) launch: Selection,
    pub(crate) running_launch_unknown: bool,
    pub(crate) generation_view_unknown: bool,
    pub(crate) at: SystemTime,
}
pub(super) fn observe(
    launch: &Selection,
    running_launch_unknown: bool,
    generation_view_unknown: bool,
) -> HeldObservation {
    HeldObservation {
        launch: launch.clone(),
        running_launch_unknown,
        generation_view_unknown,
        at: SystemTime::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pi_guest::authority::Role;

    const GATE: &str = "#[cfg(all(debug_assertions, feature = \"test-seam\"))]";

    fn launch(tag: &str) -> Selection {
        Selection {
            alias_sha256: format!("alias-{tag}"),
            generation: format!("gen-{tag}"),
            role: Role::Worker,
            model: "m".into(),
        }
    }
    fn hit_count(l: &Selection) -> usize {
        hits().iter().filter(|h| *h == l).count()
    }

    #[test]
    fn armed_seam_fires_once_for_the_armed_launch_only() {
        let (a, b) = (launch("fire-a"), launch("fire-b"));
        arm(&a).unwrap();
        assert!(before_fchmod(&b).is_ok(), "other launch unaffected");
        let err = before_fchmod(&a).unwrap_err().to_string();
        assert!(err.contains(REFUSAL_MARKER));
        assert!(before_fchmod(&a).is_ok(), "disarmed after one firing");
        assert_eq!((hit_count(&a), hit_count(&b)), (1, 0));
    }

    #[test]
    fn firing_leaves_later_calls_identical_to_production() {
        let a = launch("after");
        arm(&a).unwrap();
        assert!(before_fchmod(&a).is_err());
        for _ in 0..50 {
            assert!(before_fchmod(&a).is_ok());
        }
        assert_eq!(hit_count(&a), 1);
        // A fired launch may be armed again deliberately, and fires once more.
        arm(&a).unwrap();
        assert!(before_fchmod(&a).is_err());
        assert!(before_fchmod(&a).is_ok());
        assert_eq!(hit_count(&a), 2);
    }

    #[test]
    fn seam_never_flips_outcome_beyond_the_single_armed_refusal() {
        let (armed, other) = (launch("flip-armed"), launch("flip-other"));
        arm(&armed).unwrap();
        let outcomes: Vec<bool> = (0..20)
            .flat_map(|_| [before_fchmod(&other).is_ok(), before_fchmod(&armed).is_ok()])
            .collect();
        // Production reaches this point only after success; the sole Err is
        // the first call for the armed launch.
        assert_eq!(outcomes.iter().filter(|ok| !**ok).count(), 1);
        assert!(!outcomes[1]);
        assert!(outcomes.iter().enumerate().all(|(i, ok)| i == 1 || *ok));
        assert!(hits().iter().all(|h| *h != other));
    }

    #[test]
    fn unarmed_launch_is_always_production_and_arm_is_exact() {
        let (a, near) = (launch("exact"), {
            let mut n = launch("exact");
            n.generation.push('x');
            n
        });
        arm(&a).unwrap();
        assert!(arm(&a).is_err(), "double arm refused");
        assert!(
            before_fchmod(&near).is_ok(),
            "near-identical launch unaffected"
        );
        assert!(disarm(&a));
        assert!(!disarm(&a));
        assert!(before_fchmod(&a).is_ok());
        assert_eq!(hit_count(&a), 0);
    }

    #[test]
    fn observation_is_read_only_and_timestamped() {
        let a = launch("obs");
        arm(&a).unwrap();
        let snapshot = |s: &State| (s.armed.clone(), s.hits.clone());
        let before = snapshot(&state());
        let t0 = SystemTime::now();
        let first = observe(&a, true, true);
        let second = observe(&a, true, true);
        assert_eq!(snapshot(&state()), before, "seam state untouched");
        assert!(first.running_launch_unknown && first.generation_view_unknown);
        assert_eq!(first.launch, second.launch);
        assert!(first.at >= t0 && second.at >= first.at);
        assert!(!observe(&a, false, true).running_launch_unknown);
        assert!(disarm(&a));
    }

    /// Every seam/observation item must sit behind the exact gate.
    #[test]
    fn every_item_is_behind_the_exact_gate() {
        fn gated(source: &str, needle: &str) -> usize {
            let lines: Vec<&str> = source.lines().collect();
            let mut found = 0;
            for (i, line) in lines.iter().enumerate() {
                if !line.contains(needle) || line.trim_start().starts_with("//") {
                    continue;
                }
                found += 1;
                let mut j = i;
                let ok = loop {
                    if j == 0 {
                        break false;
                    }
                    j -= 1;
                    let t = lines[j].trim();
                    if t == GATE {
                        break true;
                    }
                    // Only doc comments and other attributes may sit between.
                    if !(t.starts_with("///") || t.starts_with("#[allow") || t.starts_with("//")) {
                        break false;
                    }
                };
                assert!(ok, "`{needle}` at line {} is not directly gated", i + 1);
            }
            found
        }
        let module = include_str!("mod.rs");
        let provision = include_str!("owner/provision.rs");
        let service = include_str!("service.rs");
        assert_eq!(gated(module, "mod diag_seam;"), 1);
        assert_eq!(gated(provision, "diag_seam::before_fchmod"), 1);
        assert_eq!(gated(provision, "fn observe_unknown"), 1);
        assert_eq!(gated(service, "fn observe_pre_teardown"), 1);
        // No other mention of the seam anywhere in these files.
        assert_eq!(module.matches("diag_seam").count(), 1);
        assert_eq!(provision.matches("diag_seam").count(), 1);
        // service.rs: return type and call, both inside the gated fn.
        assert_eq!(service.matches("diag_seam").count(), 2);
        // No env var / runtime flag reads in this module.
        let me = include_str!("diag_seam.rs");
        let code = me.split("#[cfg(test)]").next().unwrap();
        assert!(!code.contains("env::var") && !code.contains("AtomicBool"));
    }
}
