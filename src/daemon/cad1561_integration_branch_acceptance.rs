//! Independent CAD-1561 acceptance for the operator-approved
//! integration-branch delivery target.
//!
//! Every check goes through the real production path —
//! `classify`, `routine_readiness` and
//! `Shared::rpc_delivery_requirements` — against an isolated state
//! dir, PM root and `gh` executable (the cad1403 fixture shape). The
//! fake `gh` supplies deterministic transport facts only: it cannot
//! approve a policy, classify a diff or green a check the production
//! reader did not actually request. The approvals recorded here are
//! isolated-store test input — no live operator approval is claimed.
//!
//! Proven by the real guards, not by reading source or echoing values:
//! - the repository-default route is unchanged (accepts, `base_ref`
//!   binds `main`, no approval needed);
//! - a non-default PR base is admitted only while the EXACT current
//!   `delivery:` policy is operator-approved (digest/provenance match)
//!   and names that branch as `integration_branch` — then the
//!   response and the readiness evidence bind the actual target name
//!   and the routine floor reads THAT branch's rules/protection;
//! - an arbitrary target, an unapproved or digest-stale policy, a
//!   drifted file (last-approved `delivery_unapproved` fallback),
//!   a malformed section or a non-canonical branch name all refuse;
//! - the actual target's required checks cannot be substituted by the
//!   default branch's green checks;
//! - the legacy `solo_operator` v1 profile stays orthogonal to the
//!   new routing option (approved profile+target still admits and
//!   keeps routine reachable on the actual target's checks);
//! - the operator's merge-decision verbs share the same target
//!   authorization: `rpc_delivery_approve` (direct and board-relayed)
//!   and `rpc_delivery_merge check|enqueued` refuse a standing PASS
//!   on an observed open/green head whenever the PR's base is neither
//!   the repository default nor the exact approved
//!   `integration_branch` — under an unapproved, digest-stale,
//!   removed or malformed policy, under an approved custom policy
//!   that names no target, and for an arbitrary branch no approved
//!   policy names. The board/HTTP surface has no separate merge verb
//!   — the card's merge IS `delivery_approve` with a relayed
//!   `request_actor`, exercised params-level and once as a real `POST
//!   /api/delivery/<issue>/merge` behind a real daemon socket, board
//!   server and operator session. A refusal transitions nothing:
//!   the persisted record stays `passed`, no approval object is
//!   recorded, `gh pr merge` never runs and no `merge_enqueued` event
//!   is emitted.

#[cfg(all(unix, feature = "test-seam"))]
mod cad1561 {
    use super::super::{classify, routine_readiness, RoutineReadiness};
    use crate::daemon::{ServeOptions, Shared};
    use crate::delivery::{self, Record};
    use crate::issue::{self, delivery_policy};
    use crate::test_seam::{scoped, Asserted, Seam};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;

    const ISSUE: &str = "CAD-1561";
    const REPO: &str = "acme/widgets";
    const PR: u64 = 1561;
    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const MERGE_BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const REPORT: &str = "CAD-1561/reports/19700101T000000Z-worker.md";
    const DEFAULT_BRANCH: &str = "main";
    const INTEGRATION_BRANCH: &str = "staging";

    struct Fixture {
        _root: tempfile::TempDir,
        shared: std::sync::Arc<Shared>,
        pm_dir: std::path::PathBuf,
        /// The isolated daemon state dir — `delivery.json` lives here.
        dir: std::path::PathBuf,
    }

    /// `<pm>/widgets/PROJECT.md` holding one `delivery:` section.
    fn project_md(delivery_yaml: &str) -> String {
        format!("---\ndelivery:\n{delivery_yaml}---\n# Widgets\n")
    }

    /// Write the PROJECT.md a classify call will read on its next pass.
    fn write_policy(pm_dir: &std::path::Path, delivery_yaml: &str) {
        std::fs::write(
            pm_dir.join("widgets").join("PROJECT.md"),
            project_md(delivery_yaml),
        )
        .unwrap();
    }

    /// A valid section identical to the defaults plus
    /// `integration_branch: <name>` — the only admissible non-default
    /// target declaration.
    fn staging_delivery(branch: &str) -> String {
        format!(
            "  integration_branch: {branch}\n  reviews:\n    review:\n      kind: agent\n      focus: general\n  risk:\n    - require: [review]\n"
        )
    }

    /// The same approved-target section plus the legacy solo-operator
    /// profile — `integration_branch` is a routing option and must stay
    /// orthogonal to the review profile.
    fn staging_solo_delivery() -> String {
        format!(
            "  integration_branch: {INTEGRATION_BRANCH}\n  solo_operator:\n    version: 1\n  reviews:\n    review:\n      kind: agent\n      focus: general\n  risk:\n    - require: [review]\n"
        )
    }

    /// Record a `project_work_approved` event in the ISOLATED store —
    /// test input only, standing in for a real `project_work_approve`
    /// by the operator. `policy` is the validated canonical policy
    /// half the recorded digest covers.
    fn record_work_approval(shared: &Shared, policy: Option<&delivery_policy::DeliveryPolicy>) {
        let delivery_json = match policy {
            Some(p) => serde_json::to_value(p).expect("policy serializes"),
            None => serde_json::Value::Null,
        };
        let policy_for_digest = policy
            .cloned()
            .unwrap_or_else(delivery_policy::default_policy);
        shared
            .store
            .record_work_approval(json!({
                "project": "widgets",
                "digest": issue::work::gate_digest(&issue::work::WorkConfig::default()),
                "delivery": delivery_json,
                "delivery_digest": delivery_policy::digest(&policy_for_digest),
                "by": "operator",
                "at": issue::time::iso(0),
            }))
            .expect("seed isolated approval store");
    }

    /// Parse the `delivery:` section just written to the fixture
    /// PROJECT.md — the same canonical policy `classify` resolves —
    /// for building an approval's recorded halves.
    fn file_policy(pm_dir: &std::path::Path) -> delivery_policy::DeliveryPolicy {
        delivery_policy::load(pm_dir, "widgets")
            .expect("fixture delivery section must parse")
            .expect("fixture wrote a delivery section")
    }

    /// The isolated PM + fake-gh environment without any daemon —
    /// the state dir the tests boot either an in-process `Shared` or
    /// a real served daemon/board pair on. `dir` is where
    /// `delivery.json` and `cadence.sqlite3` live.
    struct Env {
        _root: tempfile::TempDir,
        pm_dir: std::path::PathBuf,
        dir: std::path::PathBuf,
    }

    /// `base_ref` is the PR's `baseRefName`; `base_rules`/
    /// `base_protection` are the rule and protection JSON the API
    /// returns for the PR's ACTUAL base branch — a reader that
    /// silently substitutes the default branch hits the distinct
    /// main-shaped arms instead. `rollup` is the PR's requested
    /// statusCheckRollup payload.
    fn fixture_env(base_ref: &str, rollup: &str, base_rules: &str, base_protection: &str) -> Env {
        let root = tempdir().expect("isolated CAD-1561 root");
        let pm_dir = root.path().join("pm");
        let pm = issue::Pm::init(&pm_dir).expect("initialize isolated PM");
        let checkout = root.path().join("widgets");
        std::fs::create_dir_all(&checkout).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widgets.git",
            ],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        issue::write::project_add(
            &pm,
            "widgets",
            "CAD",
            &[checkout.to_str().unwrap().into()],
            &[],
            &[],
            None,
        )
        .expect("register fixture project");
        issue::write::new_issue(
            &pm,
            &checkout,
            Some("widgets"),
            "Approved integration-branch target",
            None,
            None,
            &[],
            None,
            None,
            &[],
            Some(ISSUE),
            None,
            "operator",
        )
        .expect("create fixture issue");
        let report_path = pm_dir
            .join("widgets")
            .join(ISSUE)
            .join("reports")
            .join("19700101T000000Z-worker.md");
        std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
        std::fs::write(&report_path, format!("---\nschema: cadence.report/2\nkind: done\ntask: {ISSUE}\nagent: worker\nsha: {HEAD}\npr: https://github.com/{REPO}/pull/{PR}\n---\n## Expected\nready\n## Evidence\nchecks\n## Cause\n\n## Correction\n\n## Lesson\n\n## Next\n\n")).unwrap();

        let risk_paths = include_str!("../../docs/roles/risk-paths.toml");
        let one_review = include_str!("../../docs/roles/one-review-paths.toml");
        let risk_content =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, risk_paths);
        let review_content =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, one_review);
        let gh = root.path().join("gh");
        // A merge attempt — `delivery_approve`'s `gh pr merge --auto
        // … --match-head-commit` or a `delivery_observe` disable —
        // appends its argv to `merges`; a refusal leaves the file
        // absent.
        let script = r##"#!/bin/sh
here=$(dirname "$0")
if [ "$1" = pr ] && [ "$2" = merge ]; then echo "$*" >> "$here/merges"; exit 0; fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  case "$*" in
    *statusCheckRollup*) rollup=',"statusCheckRollup":[%ROLLUP%]' ;;
    *) rollup='' ;;
  esac
  printf '{"number":%PR%,"state":"OPEN","title":"CAD-1561: fixture","headRefOid":"%HEAD%","baseRefName":"%BRANCH%","baseRefOid":"%OID%","changedFiles":1%s}\n' "$rollup"
  exit 0
fi
if [ "$1" = repo ] && [ "$2" = view ]; then printf '{"defaultBranchRef":{"name":"%DEF%"}}\n'; exit 0; fi
if [ "$1" = api ]; then
  endpoint=$2
  case "$endpoint" in
    repos/%REPO%/compare/%OID%...%HEAD%) printf '{"merge_base_commit":{"sha":"%MBASE%"},"files":[{"filename":"docs/guide.md","status":"modified","additions":1,"deletions":0}]}\n' ;;
    repos/%REPO%/git/trees/%MBASE%?recursive=1|repos/%REPO%/git/trees/%HEAD%?recursive=1) printf '{"truncated":false,"tree":[{"path":"docs/guide.md","mode":"100644","type":"blob"}]}\n' ;;
    repos/%REPO%/contents/docs/roles/risk-paths.toml\?ref=%OID%) printf '{"encoding":"base64","content":"%RISK%"}\n' ;;
    repos/%REPO%/contents/docs/roles/one-review-paths.toml\?ref=%OID%) printf '{"encoding":"base64","content":"%REVIEW%"}\n' ;;
    # The default branch's own rules/protection: required context "ci".
    # A reader binding the default branch instead of the actual target
    # reads THESE arms — exactly the substitution refused below.
    repos/%REPO%/rules/branches/%DEF%) printf '[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci"}]}}]\n' ;;
    repos/%REPO%/branches/%DEF%/protection) printf '{"required_status_checks":{"contexts":["ci"],"checks":[]}}\n' ;;
    # The PR's actual target branch, whatever it is named.
    repos/%REPO%/rules/branches/%BRANCH%) printf '%s\n' '%BRULES%' ;;
    repos/%REPO%/branches/%BRANCH%/protection) printf '%s\n' '%BPROT%' ;;
    repos/%REPO%/actions/workflows/ci.yml/runs*) printf '{"workflow_runs":[{"id":1561,"head_sha":"%HEAD%","event":"pull_request","status":"completed","conclusion":"success","html_url":"https://github.com/%REPO%/actions/runs/1561"}]}\n' ;;
    *) echo "unhandled API fixture: $endpoint" >&2; exit 1 ;;
  esac
  exit 0
fi
echo "unhandled gh fixture: $*" >&2
exit 1
"##
        .replace("%ROLLUP%", rollup)
        .replace("%BRULES%", base_rules)
        .replace("%BPROT%", base_protection)
        .replace("%BRANCH%", base_ref)
        .replace("%MBASE%", MERGE_BASE)
        .replace("%DEF%", DEFAULT_BRANCH)
        .replace("%REPO%", REPO)
        .replace("%RISK%", &risk_content)
        .replace("%REVIEW%", &review_content)
        .replace("%PR%", &PR.to_string())
        .replace("%HEAD%", HEAD)
        .replace("%OID%", BASE);
        std::fs::write(&gh, script).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut record = Record::new(ISSUE, "widgets", "worker", 0);
        record.dispatched_at = 0;
        record.pr = Some(format!("https://github.com/{REPO}/pull/{PR}"));
        record.head = Some(HEAD.into());
        record.outcome_report = Some(REPORT.into());
        let mut records = BTreeMap::new();
        records.insert(ISSUE.into(), record);
        delivery::save(root.path(), &records).expect("save worker record");

        Env {
            pm_dir,
            dir: root.path().to_path_buf(),
            _root: root,
        }
    }

    /// The isolated environment plus an in-process `Shared` for the
    /// direct-RPC tests.
    fn fixture(base_ref: &str, rollup: &str, base_rules: &str, base_protection: &str) -> Fixture {
        let Env {
            _root: root,
            pm_dir,
            dir,
        } = fixture_env(base_ref, rollup, base_rules, base_protection);
        let opts = ServeOptions {
            test_seam: true,
            delivery_gh: Some(dir.join("gh")),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let shared = Shared::new(&dir, &opts).expect("initialize isolated daemon");
        Fixture {
            pm_dir,
            shared,
            dir,
            _root: root,
        }
    }

    fn pr_url() -> String {
        format!("https://github.com/{REPO}/pull/{PR}")
    }

    fn request() -> serde_json::Value {
        json!({"issue": ISSUE, "pr": pr_url(), "head": HEAD})
    }

    /// `Err` or a non-`Ready` outcome both refuse; only `Ready` admits.
    fn readiness_refused(result: &crate::error::Result<RoutineReadiness>) -> bool {
        !matches!(result, Ok(RoutineReadiness::Ready(_)))
    }

    fn readiness_name(result: &crate::error::Result<RoutineReadiness>) -> &'static str {
        match result {
            Ok(RoutineReadiness::Ready(_)) => "Ready",
            Ok(RoutineReadiness::Waiting) => "Waiting",
            Ok(RoutineReadiness::NotRoutine) => "NotRoutine",
            Err(_) => "Err",
        }
    }

    /// A custom, non-default-equivalent `delivery:` section naming NO
    /// integration target — the approved `NoProfile` activation case.
    /// `max_revise` is the only difference from the defaults, so the
    /// section is a valid approvable policy that still authorizes no
    /// non-default branch.
    fn custom_untargeted_delivery() -> String {
        "  max_revise: 4\n  reviews:\n    review:\n      kind: agent\n      focus: general\n  risk:\n    - require: [review]\n".to_string()
    }

    /// Seed the isolated `delivery.json` with a standing PASS on HEAD:
    /// the recorded verdict is `pass` on the observed, OPEN, green
    /// head, so `Record::merge_ready` holds — the exact precondition
    /// both merge-decision verbs checked before CAD-1561. Seeded test
    /// input only; no live operator approval is claimed. Re-running it
    /// also restores a record a prior buggy-path call transitioned.
    fn seed_passed(dir: &std::path::Path) {
        let mut all = delivery::load(dir).expect("load isolated delivery records");
        let rec = all.get_mut(ISSUE).expect("fixture delivery record exists");
        rec.verdict = Some(delivery::VerdictRec {
            verdict: "pass".into(),
            sha: HEAD.into(),
            reviewer: "fixture-reviewer".into(),
            summary: "independent review passed".into(),
            report: format!("{ISSUE}/reports/19700101T000000Z-worker.md"),
            at: 1,
        });
        rec.observed = Some(delivery::Observed {
            head: HEAD.into(),
            pr_state: "OPEN".into(),
            ci_green: true,
            ..Default::default()
        });
        rec.enter(delivery::State::Passed, 1);
        assert!(rec.merge_ready(), "fixture PASS must be merge-ready");
        delivery::save(dir, &all).expect("save PASS record");
    }

    /// The persisted loop state of the fixture record — a refusal
    /// leaves it `passed`.
    fn persisted_state(dir: &std::path::Path) -> delivery::State {
        delivery::load(dir)
            .expect("load isolated delivery records")
            .get(ISSUE)
            .map(|r| r.state)
            .expect("fixture delivery record exists")
    }

    /// The fake `gh`'s merge argv log — absent until some call runs
    /// `pr merge`.
    fn merges_log(dir: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(dir.join("merges")).ok()
    }

    /// Seq of the newest `approval_recorded` / `merge_enqueued` events —
    /// a refusal advances neither.
    fn event_seqs(f: &Fixture) -> (Option<i64>, Option<i64>) {
        let approval = f
            .shared
            .store
            .last_event_of(
                crate::store::APPROVAL_STREAM,
                &[crate::store::APPROVAL_RECORDED_EVENT],
            )
            .expect("read approval stream")
            .map(|e| e.seq);
        let enqueued = f
            .shared
            .store
            .last_event_of(crate::store::Store::DAEMON_STREAM, &["merge_enqueued"])
            .expect("read daemon stream")
            .map(|e| e.seq);
        (approval, enqueued)
    }

    /// Read-only counts of `approval_recorded` and `merge_enqueued`
    /// events in the fixture's store file — used where the daemon
    /// under test owns the `Store` handle (the HTTP/board case), so a
    /// second read-only connection never writes or recovers it.
    fn event_counts(dir: &std::path::Path) -> (i64, i64) {
        let db = rusqlite::Connection::open_with_flags(
            dir.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("open fixture store read-only");
        let count =
            |sql: &str| -> i64 { db.query_row(sql, [], |r| r.get(0)).expect("count events") };
        (
            count("SELECT count(*) FROM events WHERE alias = 'audit:approvals' AND kind = 'approval_recorded'"),
            count("SELECT count(*) FROM events WHERE alias = 'daemon' AND kind = 'merge_enqueued'"),
        )
    }

    /// Stops and joins the spawned daemon/board threads when a test
    /// ends (the cad1218 harness's shape).
    struct BoardStop(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

    impl Drop for BoardStop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            for thread in self.1.drain(..).rev() {
                let _ = thread.join();
            }
        }
    }

    /// The credentials a browser tab holds after `/api/session`:
    /// the HttpOnly cookie and the page's session key, sent on every
    /// board write together.
    struct BoardSession {
        cookie: String,
        key: String,
    }

    /// One board write, byte for byte what the card's tab sends —
    /// host, board header, Origin, the session cookie+key, and the
    /// seam-asserted peer identity.
    fn board_post(
        agent: &ureq::Agent,
        base: &str,
        host: &str,
        token: &str,
        session: &BoardSession,
        path: &str,
        body: &str,
    ) -> ureq::http::Response<ureq::Body> {
        agent
            .post(format!("{base}{path}"))
            .header("Host", host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, token)
            .header("Cookie", &session.cookie)
            .header("X-Cadence-Session", &session.key)
            .header("Content-Type", "application/json")
            .send(body.to_string())
            .expect("board request must be answered")
    }

    /// One merge-decision call that must refuse without any mutation:
    /// the call itself is `Err`, the persisted record stays `passed`,
    /// no approval event is recorded, `gh pr merge` never runs and no
    /// `merge_enqueued` event is emitted.
    fn assert_refused(
        f: &Fixture,
        label: &str,
        call: impl FnOnce() -> crate::error::Result<serde_json::Value>,
    ) {
        seed_passed(&f.dir);
        let (approval_before, enqueued_before) = event_seqs(f);
        let merges_before = merges_log(&f.dir);
        let result = call();
        assert!(result.is_err(), "{label} must refuse, got {result:?}");
        let (approval_after, enqueued_after) = event_seqs(f);
        assert_eq!(
            approval_after, approval_before,
            "{label} must record no approval"
        );
        assert_eq!(
            enqueued_after, enqueued_before,
            "{label} must emit no merge_enqueued event"
        );
        assert_eq!(
            merges_log(&f.dir),
            merges_before,
            "{label} must not invoke gh pr merge"
        );
        assert_eq!(
            persisted_state(&f.dir),
            delivery::State::Passed,
            "{label} must leave the record passed"
        );
    }

    /// The direct (CLI-shaped) `delivery_approve`, the board/HTTP
    /// relay (`request_actor` — the loopback card's `operator (ui)`)
    /// and the retained `delivery_merge check`/`enqueued` phases over
    /// the same standing PASS.
    fn refuse_all(f: &Fixture, label: &str) {
        let shared = f.shared.clone();
        assert_refused(f, &format!("{label}: delivery_approve"), move || {
            scoped(Asserted::Operator, || {
                shared
                    .rpc_delivery_approve(&json!({"issue": ISSUE, "sha": HEAD}), std::process::id())
            })
        });
        let shared = f.shared.clone();
        assert_refused(
            f,
            &format!("{label}: board-relayed delivery_approve"),
            move || {
                scoped(Asserted::Operator, || {
                    shared.rpc_delivery_approve(
                        &json!({"issue": ISSUE, "sha": HEAD, "request_actor": "operator (ui)"}),
                        std::process::id(),
                    )
                })
            },
        );
        let shared = f.shared.clone();
        assert_refused(f, &format!("{label}: delivery_merge check"), move || {
            scoped(Asserted::Operator, || {
                shared.rpc_delivery_merge(
                    &json!({"issue": ISSUE, "phase": "check"}),
                    std::process::id(),
                )
            })
        });
        let shared = f.shared.clone();
        assert_refused(f, &format!("{label}: delivery_merge enqueued"), move || {
            scoped(Asserted::Operator, || {
                shared.rpc_delivery_merge(
                    &json!({"issue": ISSUE, "phase": "enqueued", "sha": HEAD}),
                    std::process::id(),
                )
            })
        });
    }

    /// Baseline: the repository-default route is unchanged. A `main`
    /// PR classifies with `base_ref` bound to `main` under no
    /// approval, and a routine docs diff readies on the default
    /// branch's own checks.
    #[test]
    fn cad1561_default_branch_route_is_unchanged() {
        let f = fixture(
            DEFAULT_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"}"#,
            "[]",
            "{}",
        );
        let answer = classify(&f.shared, &request()).expect("default-branch PR must classify");
        assert_eq!(answer["base"], BASE, "base stays the exact base OID");
        assert_eq!(
            answer["base_ref"], DEFAULT_BRANCH,
            "the response must bind the actual target name: {answer}"
        );
        let ready = routine_readiness(&f.shared, ISSUE, &pr_url(), HEAD, REPORT);
        assert!(
            matches!(ready, Ok(RoutineReadiness::Ready(_))),
            "routine docs diff on the default branch readies on its own checks: {}",
            readiness_name(&ready)
        );
    }

    /// Admission + target binding: a PR to `staging` is admitted while
    /// the exact current policy — canonical digest equal to the
    /// recorded operator approval — names `integration_branch:
    /// staging`. The response binds `base_ref: staging` and the
    /// routine floor evaluates the STAGING branch's own required check
    /// `ci-staging`, not main's `ci`. `main` green checks are present
    /// in the rollup and remain irrelevant to the staging gate.
    #[test]
    fn cad1561_approved_integration_branch_binds_actual_target() {
        // `main` green, staging's required context green: only the
        // actual target's rules can produce Ready here.
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&policy));

        let answer = classify(&f.shared, &request())
            .expect("freshly approved integration target must classify");
        assert_eq!(
            answer["base_ref"], INTEGRATION_BRANCH,
            "the response must name the actual approved target: {answer}"
        );
        assert_eq!(answer["base"], BASE);
        assert_eq!(
            answer["requirements"]["policy_digest"], answer["policy_digest"],
            "requirements bind the approved policy digest"
        );

        // A correct implementation reads staging's rules (requiring
        // `ci-staging`, which is green); one that substituted main's
        // `ci` would still see it green — the missing/failing rows
        // below disprove the substitution in the other direction.
        let ready = routine_readiness(&f.shared, ISSUE, &pr_url(), HEAD, REPORT);
        let evidence = match ready {
            Ok(RoutineReadiness::Ready(e)) => e,
            other => panic!(
                "staging's own green required check must ready the routine floor: {}",
                readiness_name(&other)
            ),
        };
        let evidence_json = serde_json::to_value(&evidence).unwrap();
        assert_eq!(
            evidence_json["base_ref"], INTEGRATION_BRANCH,
            "readiness evidence must bind the actual target name: {evidence_json}"
        );
        assert_eq!(evidence.base, BASE, "evidence binds the exact base OID");
        assert_eq!(
            evidence.policy_digest,
            answer["policy_digest"].as_str().unwrap(),
            "evidence binds the approved policy digest"
        );

        // The public RPC surface carries the same binding.
        let rpc = f
            .shared
            .rpc_delivery_requirements(&request())
            .expect("requirements RPC");
        assert_eq!(rpc["base_ref"], INTEGRATION_BRANCH, "wire binds base_ref");
        assert_eq!(
            rpc["readiness"]["base_ref"], INTEGRATION_BRANCH,
            "wire readiness binds the actual target name: {rpc}"
        );
    }

    /// Legacy profile compatibility: `solo_operator: {version: 1}` is
    /// orthogonal to the new routing option. The approved policy below
    /// is default-equivalent plus `integration_branch: staging` plus
    /// the supported profile — it must validate, admit the staging
    /// target and keep the routine class reachable on the staging
    /// branch's own checks, exactly as the profile does on `main`.
    #[test]
    fn cad1561_solo_operator_profile_stays_compatible_with_integration_target() {
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&f.pm_dir, &staging_solo_delivery());
        let policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&policy));

        let answer =
            classify(&f.shared, &request()).expect("approved profile+target policy must classify");
        assert_eq!(answer["base_ref"], INTEGRATION_BRANCH);
        assert_eq!(
            answer["requirements"]["class"], "routine",
            "the supported profile keeps routine reachable on the approved target: {answer}"
        );
        let ready = routine_readiness(&f.shared, ISSUE, &pr_url(), HEAD, REPORT);
        assert!(
            matches!(ready, Ok(RoutineReadiness::Ready(_))),
            "profile+target policy readies on staging's own green check: {}",
            readiness_name(&ready)
        );
    }

    /// Refusal of every non-approved route to a non-default base:
    /// - arbitrary: the PR targets `release-candidate`, which no valid
    ///   current approved policy names;
    /// - unapproved: the file names `staging` with no recorded approval
    ///   (the section is not the default → `delivery_unapproved`, the
    ///   default applies);
    /// - digest-stale: an approval exists but for a DIFFERENT policy
    ///   (`integration_branch: release-candidate`) than the current
    ///   file;
    /// - removed: the approved section was deleted — the last-approved
    ///   `delivery_unapproved` fallback still reports
    ///   `source=approved` yet carries a note, and must NOT admit;
    /// - malformed section and non-canonical branch names: the file
    ///   cannot even be a valid current policy.
    ///
    /// In every case the resolved policy still requires the default
    /// branch, or the binding approval is absent/not-current, so
    /// `staging` must refuse.
    #[test]
    fn cad1561_unapproved_arbitrary_malformed_and_drifted_targets_refuse() {
        // The whole row set is refused on ONE fixture whose PR targets
        // staging with staging's own checks green — so every refusal is
        // a target-selection refusal, never a missing-check accident.
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );

        // Unapproved file: custom section, no approval at all.
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        assert!(
            classify(&f.shared, &request()).is_err(),
            "an unapproved custom policy must not admit a non-default target"
        );
        assert!(
            readiness_refused(&routine_readiness(
                &f.shared,
                ISSUE,
                &pr_url(),
                HEAD,
                REPORT
            )),
            "an unapproved custom policy must not ready a non-default target"
        );

        // Digest-stale: approval names release-candidate; the file says
        // staging. The approved fallback stays in force with a note
        // and must not admit staging.
        write_policy(&f.pm_dir, &staging_delivery("release-candidate"));
        let stale_policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&stale_policy));
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        assert!(
            classify(&f.shared, &request()).is_err(),
            "a policy digest-stale against its approval must not admit"
        );

        // Removed section: the recorded approval IS the staging
        // policy and the file is deleted afterward, so the resolver
        // keeps the approved policy in force under
        // `delivery_unapproved` — the fallback would authorize staging
        // if a note were honored; it must not be.
        let staging_policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&staging_policy));
        std::fs::remove_file(f.pm_dir.join("widgets").join("PROJECT.md")).unwrap();
        assert!(
            classify(&f.shared, &request()).is_err(),
            "the last-approved fallback (note present) must not admit a non-default target"
        );
        assert!(
            readiness_refused(&routine_readiness(
                &f.shared,
                ISSUE,
                &pr_url(),
                HEAD,
                REPORT
            )),
            "the fallback must not ready a non-default target"
        );

        // Malformed section: not valid YAML — with the staging
        // approval still on record even a resolving fallback must not
        // admit; the current file is not a valid approved policy.
        write_policy(&f.pm_dir, "  integration_branch: [unclosed\n");
        assert!(
            classify(&f.shared, &request()).is_err(),
            "a malformed delivery section must refuse a non-default target"
        );

        // Non-canonical branch names in a current file: ref syntax,
        // traversal, revision and glob forms are never valid targets.
        // `load` must refuse the NAME at parse — without that first
        // assertion a wrongly-accepted name could still be refused only
        // for lacking an approval and the parser defect would hide.
        for name in [
            "refs/heads/staging",
            "../staging",
            "staging@{1}",
            "feature*",
            "-staging",
            "staging.lock",
            "feature.lock/staging",
            "feature..x",
        ] {
            write_policy(&f.pm_dir, &staging_delivery(name));
            assert!(
                delivery_policy::load(&f.pm_dir, "widgets").is_err(),
                "the parser must refuse non-canonical branch name {name:?}"
            );
            assert!(
                classify(&f.shared, &request()).is_err(),
                "non-canonical branch name {name:?} must refuse"
            );
        }

        // Arbitrary target: the approved policy names staging, but a
        // PR to a different branch is not covered by it.
        let other = fixture(
            "release-candidate",
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&other.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let approved = file_policy(&other.pm_dir);
        record_work_approval(&other.shared, Some(&approved));
        assert!(
            classify(&other.shared, &request()).is_err(),
            "an approved staging policy must not admit an arbitrary other branch"
        );
        assert!(
            readiness_refused(&routine_readiness(
                &other.shared,
                ISSUE,
                &pr_url(),
                HEAD,
                REPORT
            )),
            "an arbitrary branch must not ready"
        );
    }

    /// No substitution of default-branch checks for actual-target
    /// checks. The PR targets staging; `main` requires `ci` and its
    /// rollup entry is SUCCESS — while staging's OWN required
    /// `ci-staging` is absent or failing. A reader that consulted the
    /// default branch would wrongly find a green floor; the real
    /// guard must not admit.
    #[test]
    fn cad1561_actual_target_checks_cannot_substitute_default_branch_checks() {
        // Missing: staging's required context never reported.
        let missing = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&missing.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let policy = file_policy(&missing.pm_dir);
        record_work_approval(&missing.shared, Some(&policy));
        let ready = routine_readiness(&missing.shared, ISSUE, &pr_url(), HEAD, REPORT);
        assert!(
            readiness_refused(&ready),
            "missing staging required check must not ready on main's green ci: {}",
            readiness_name(&ready)
        );
        let rpc = missing
            .shared
            .rpc_delivery_requirements(&request())
            .expect("requirements RPC");
        assert!(
            rpc["readiness"].is_null(),
            "wire readiness must not fabricate a staging pass: {rpc}"
        );

        // Failing: staging's required context reported FAILURE.
        let failing = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"FAILURE"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&failing.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let policy = file_policy(&failing.pm_dir);
        record_work_approval(&failing.shared, Some(&policy));
        let ready = routine_readiness(&failing.shared, ISSUE, &pr_url(), HEAD, REPORT);
        assert!(
            readiness_refused(&ready),
            "failing staging required check must refuse: {}",
            readiness_name(&ready)
        );
    }

    /// The operator's merge-decision verbs share the target
    /// authorization `classify`/`routine_readiness` enforce. A record
    /// holding a standing PASS on an observed open/green head is
    /// exactly what `merge_ready` admits, yet the merge must still
    /// refuse whenever the PR's base is not authorized by the CURRENT
    /// approved policy:
    ///
    /// - unapproved: the file names `staging` but no approval stands —
    ///   the `delivery_unapproved` default fallback authorizes only
    ///   `main`;
    /// - digest-stale: the recorded approval names `release-candidate`
    ///   while the file names `staging` — the last-approved fallback
    ///   carries a note and must not admit;
    /// - removed: the approved staging section was deleted — the
    ///   fallback reports `source=approved` with a note and must not
    ///   admit;
    /// - malformed: the section cannot parse — with the stale approval
    ///   standing the fallback stays noted, and on a fixture with NO
    ///   recorded approval at all the `delivery_error` default refuses;
    /// - NoProfile: an approved custom policy (`max_revise` differs
    ///   from the defaults) names no `integration_branch` — a staging
    ///   PR is outside it;
    /// - arbitrary: the approved policy names `staging` but the PR
    ///   targets `release-candidate`, which nothing authorizes.
    ///
    /// Every refusal is exercised through the real
    /// `rpc_delivery_approve` — direct (CLI shape) and board-relayed
    /// (`request_actor: operator (ui)`, the loopback card's actor —
    /// the board/HTTP surface relays merges through this one verb) —
    /// and the retained `rpc_delivery_merge check`/`enqueued` phases,
    /// each against the same standing-PASS record. The refusal must
    /// write nothing: no approval object, no `gh pr merge`, no
    /// `merge_enqueued` event, and the persisted record stays
    /// `passed`.
    #[test]
    fn cad1561_approve_and_merge_refuse_unauthorized_targets() {
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );

        // Unapproved custom policy on the file.
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        refuse_all(&f, "unapproved staging policy");

        // Digest-stale: approval recorded for a release-candidate
        // policy, then the file changed to staging.
        write_policy(&f.pm_dir, &staging_delivery("release-candidate"));
        let stale_policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&stale_policy));
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        refuse_all(&f, "digest-stale approval");

        // Removed: the approved staging section is deleted — the
        // last-approved fallback is noted and must not admit.
        let staging_policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&staging_policy));
        std::fs::remove_file(f.pm_dir.join("widgets").join("PROJECT.md")).unwrap();
        refuse_all(&f, "removed delivery section");

        // Malformed while the staging approval still stands.
        write_policy(&f.pm_dir, "  integration_branch: [unclosed\n");
        refuse_all(&f, "malformed delivery section");

        // Malformed on a fixture that never recorded an approval — the
        // `delivery_error` default fallback, no approved target at all.
        let bare = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&bare.pm_dir, "  integration_branch: [unclosed\n");
        refuse_all(&bare, "malformed section without any approval");

        // NoProfile: a valid approved custom policy that names no
        // integration target.
        write_policy(&bare.pm_dir, &custom_untargeted_delivery());
        let custom_policy = file_policy(&bare.pm_dir);
        record_work_approval(&bare.shared, Some(&custom_policy));
        refuse_all(&bare, "approved custom policy naming no target");

        // Arbitrary: the approved staging policy does not name the
        // branch this PR targets.
        let other = fixture(
            "release-candidate",
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&other.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let approved = file_policy(&other.pm_dir);
        record_work_approval(&other.shared, Some(&approved));
        refuse_all(&other, "approved staging policy, arbitrary PR base");
    }

    /// The positive controls: the same standing PASS over the same
    /// fixture shape is still admitted when the target IS authorized —
    /// the repository default under no policy, and the approved
    /// `integration_branch` under its exact approved policy. These
    /// distinguish a target-authorization refusal from a broken
    /// fixture or a guard that refuses every merge. `approve` on the
    /// authorized records actually enqueues (asserted via the
    /// persisted state and the merge argv the fake `gh` logged); the
    /// legacy check/enqueued phases admit the approved target too.
    #[test]
    fn cad1561_approve_and_merge_admit_authorized_targets() {
        // Repository default: `delivery_approve` on a `main` PR
        // under no delivery section keeps working end to end.
        let f = fixture(
            DEFAULT_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"}"#,
            "[]",
            "{}",
        );
        seed_passed(&f.dir);
        let shared = f.shared.clone();
        let out = scoped(Asserted::Operator, || {
            shared.rpc_delivery_approve(&json!({"issue": ISSUE, "sha": HEAD}), std::process::id())
        })
        .expect("the default-branch merge approval must admit");
        assert_eq!(
            out["state"], "enqueued",
            "an authorized approve enqueues the merge: {out}"
        );
        assert_eq!(
            persisted_state(&f.dir),
            delivery::State::Enqueued,
            "the persisted record shows the enqueue"
        );
        assert!(
            merges_log(&f.dir)
                .is_some_and(|log| log.contains("--match-head-commit") && log.contains(HEAD)),
            "the enqueue pinned the approved head"
        );

        // Approved non-default target: the exact approved staging
        // policy admits `delivery_approve` on the staging PR — the
        // guard is target authorization, not a non-default refusal.
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&policy));
        seed_passed(&f.dir);
        let shared = f.shared.clone();
        let out = scoped(Asserted::Operator, || {
            shared.rpc_delivery_approve(&json!({"issue": ISSUE, "sha": HEAD}), std::process::id())
        })
        .expect("the approved integration target must admit the merge approval");
        assert_eq!(out["state"], "enqueued", "authorized target: {out}");

        // Legacy custom profile on the default branch: a valid,
        // approved non-default-equivalent (`NoProfile`) policy that
        // names no target still admits a `main` PR — the guard must
        // not refuse every non-`Active` activation.
        let f = fixture(
            DEFAULT_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"}"#,
            "[]",
            "{}",
        );
        write_policy(&f.pm_dir, &custom_untargeted_delivery());
        let policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&policy));
        seed_passed(&f.dir);
        let shared = f.shared.clone();
        let out = scoped(Asserted::Operator, || {
            shared.rpc_delivery_approve(&json!({"issue": ISSUE, "sha": HEAD}), std::process::id())
        })
        .expect("an approved custom policy must admit the default-branch merge");
        assert_eq!(
            out["state"], "enqueued",
            "default branch under an approved NoProfile policy: {out}"
        );

        // The retained legacy phases on the same approved target:
        // `check` answers the pinned head and `enqueued` transitions.
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&policy));
        seed_passed(&f.dir);
        let shared = f.shared.clone();
        let out = scoped(Asserted::Operator, || {
            shared.rpc_delivery_merge(
                &json!({"issue": ISSUE, "phase": "check"}),
                std::process::id(),
            )
        })
        .expect("the approved integration target must admit check");
        assert_eq!(out["sha"], HEAD, "check answers the pinned head: {out}");
        let shared = f.shared.clone();
        scoped(Asserted::Operator, || {
            shared.rpc_delivery_merge(
                &json!({"issue": ISSUE, "phase": "enqueued", "sha": HEAD}),
                std::process::id(),
            )
        })
        .expect("the approved integration target must admit enqueued");
        assert_eq!(persisted_state(&f.dir), delivery::State::Enqueued);
    }

    /// The same target authorization over the actual HTTP peer: a
    /// real daemon on its own socket behind a real board server, and
    /// a real `POST /api/delivery/<issue>/merge` carrying a real
    /// operator session — not a params-level call. The card relays
    /// `delivery_approve`; the unauthorized staging target must be
    /// refused with nothing recorded, nothing merged and the record
    /// still `passed`. Approving the exact policy through the real
    /// `project_work_approve` RPC flips the same row to admitted —
    /// the same session, request and endpoint, so only the target
    /// authorization differs. The seam stands in for `/proc`
    /// ancestry only; session, write guards and the relay run
    /// unmodified.
    #[test]
    fn cad1561_board_merge_request_refuses_unauthorized_target() {
        let env = fixture_env(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        // The staging policy exists on disk but is not approved: the
        // `delivery_unapproved` default fallback authorizes only the
        // repository default.
        write_policy(&env.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        seed_passed(&env.dir);

        let mut stop = BoardStop(Arc::new(AtomicBool::new(false)), Vec::new());
        // The daemon this board relays to — the real serve loop on
        // the fixture's socket.
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.0.clone()),
            delivery_gh: Some(env.dir.join("gh")),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", env.pm_dir.to_str().unwrap());
        let state = env.dir.clone();
        stop.1.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&state, opts).unwrap()
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !env.dir.join("cadence.sock").exists() || Seam::token_at(&env.dir).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "test daemon did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let token = Seam::token_at(&env.dir).unwrap();

        // The board itself — a real tiny_http server; the first free
        // port in the sanctioned isolated test range (never the
        // production 3010 default).
        let mut port = 3110 + (std::process::id() % 80) as u16;
        loop {
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                // The board's own GitHub executable (delivery sync)
                // must be the fixture fake too — never a real `gh`
                // on PATH — and the interval sits past the test's
                // lifetime so no background pass interferes.
                gh: Some(env.dir.join("gh")),
                delivery_sync_every: Some(Duration::from_secs(600)),
                stop: Some(stop.0.clone()),
                startup: Some(startup),
                test_seam: true,
                ..Default::default()
            };
            let (s, p) = (env.dir.clone(), env.pm_dir.clone());
            let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &opts)));
            match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
                Ok(()) => {
                    stop.1.push(thread);
                    break;
                }
                Err(std::io::ErrorKind::AddrInUse) if port < 3199 => {
                    port += 1;
                    thread.join().unwrap();
                }
                Err(kind) => panic!("board could not bind: {kind:?}"),
            }
        }
        crate::operator_auth::ensure_secret(&env.dir).unwrap();

        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let base = format!("http://127.0.0.1:{port}");
        let host = format!("cadence-{port}.localhost:{port}");

        // A real operator session through the real login exchange.
        let secret = crate::operator_auth::read_secret(&env.dir).unwrap();
        let nonce = scoped(Asserted::Operator, || {
            crate::client::rpc(
                &env.dir,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap()["nonce"]
            .clone();
        let response = agent
            .post(format!("{base}/api/session"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &token)
            .header("Content-Type", "application/json")
            .send(json!({"nonce": nonce}).to_string())
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            200,
            "the operator session exchange must succeed"
        );
        let set_cookie = response.headers()["set-cookie"].to_str().unwrap();
        let cookie = set_cookie[..set_cookie.find(';').unwrap()].to_owned();
        let login: Value = response.into_body().read_json().unwrap();
        let session = BoardSession {
            cookie,
            key: login["session_key"].as_str().unwrap().to_string(),
        };

        let merge = format!("/api/delivery/{ISSUE}/merge");
        let body = json!({"sha": HEAD}).to_string();
        let (approvals_before, enqueued_before) = event_counts(&env.dir);

        // The actual HTTP request against the unauthorized target:
        // refused over the wire with no approval, no merge and the
        // record still `passed`.
        let refused = board_post(&agent, &base, &host, &token, &session, &merge, &body);
        let status = refused.status().as_u16();
        assert!(
            (400..500).contains(&status),
            "the unapproved target must refuse over HTTP, got {status}"
        );
        assert_eq!(
            persisted_state(&env.dir),
            delivery::State::Passed,
            "the HTTP refusal must leave the record passed"
        );
        assert_eq!(
            event_counts(&env.dir),
            (approvals_before, enqueued_before),
            "the HTTP refusal must record no approval and no merge_enqueued"
        );
        assert_eq!(
            merges_log(&env.dir),
            None,
            "the HTTP refusal must not invoke gh pr merge"
        );

        // Control: approve the exact current policy through the real
        // operator approval RPC, then the SAME request over the SAME
        // session admits — so the refusal above is the target
        // authorization, not a bad fixture, session or body.
        scoped(Asserted::Operator, || {
            crate::client::rpc(
                &env.dir,
                "project_work_approve",
                json!({"project": "widgets"}),
            )
        })
        .expect("the staging policy must approve through the real verb");
        let admitted = board_post(&agent, &base, &host, &token, &session, &merge, &body);
        assert_eq!(
            admitted.status().as_u16(),
            200,
            "the approved target must admit the same board merge"
        );
        assert_eq!(
            persisted_state(&env.dir),
            delivery::State::Enqueued,
            "the admitted board merge enqueues"
        );
        let (approvals_after, enqueued_after) = event_counts(&env.dir);
        assert_eq!(
            approvals_after,
            approvals_before + 1,
            "the admitted board merge records its approval"
        );
        assert_eq!(
            enqueued_after,
            enqueued_before + 1,
            "the admitted board merge emits merge_enqueued"
        );
        assert!(
            merges_log(&env.dir)
                .is_some_and(|log| log.contains("--match-head-commit") && log.contains(HEAD)),
            "the admitted board merge pinned the approved head"
        );

        drop(stop);
    }
}
