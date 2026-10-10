//! CAD-1318 independent acceptance check (written from the ticket by the
//! acceptance author, not the implementer; the implementer may not edit or
//! weaken it).
//!
//! The real guard under test is the installed-app read itself —
//! `issue::app_catalog::workspace::list`, the one function behind the CLI's
//! `app catalog ls`, the daemon's `app_workspace_list` RPC and the board's
//! `/api/app-installations` route. Per the ticket's outcome, a genuinely
//! never-initialized workspace (no `.apps/catalog.yaml`, no retained
//! installation or recovery artifacts, no pending transaction, no
//! unmigrated legacy state) lists `[]` instead of failing — and repeated
//! reads stay read-only: no catalog file, no journal, no grant or PM
//! mutation is created merely to render an empty list. Every damaged or
//! unexplained storage state the ticket enumerates — a retained pending
//! transaction, an orphaned installation or journal artifact, unexplained
//! `.apps` contents, a malformed published catalog, unmigrated legacy
//! installs and a lock/generation inconsistency — must still refuse, never
//! be smoothed into an empty array.

#![cfg(all(unix, not(feature = "e2e")))]

use cadence_agent::issue::app_catalog::workspace;
use cadence_agent::issue::Pm;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A PM init'ed through the real `Pm::init` — same skeleton `issue init`
/// produces, with no `.apps` registry at all. `Pm::init` itself already
/// commits, so a later planted state does not move HEAD.
struct Case {
    _tmp: tempfile::TempDir,
    pm_dir: PathBuf,
    pm: Pm,
}

impl Case {
    fn fresh() -> Self {
        let tmp = tempfile::Builder::new().prefix("c1318").tempdir().unwrap();
        let pm_dir = tmp.path().join("pm");
        let pm = Pm::init(&pm_dir).unwrap();
        assert!(
            !pm_dir.join(".apps").exists(),
            "Pm::init must not bootstrap an app registry"
        );
        Self {
            _tmp: tmp,
            pm_dir,
            pm,
        }
    }

    fn list(&self) -> cadence_agent::Result<Value> {
        workspace::list(&self.pm)
    }

    /// Every entry of `.apps` with its bytes, plus the git HEAD —
    /// the read-only and refusal no-mutation proof. `symlink_metadata`
    /// throughout: a planted symlink is recorded as `name -> target` and
    /// never traversed, so the snapshot cannot leave the tracker.
    fn state(&self) -> BTreeMap<String, Vec<u8>> {
        fn entry(base: &Path, p: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            let meta = std::fs::symlink_metadata(p).unwrap();
            if meta.file_type().is_symlink() {
                let target = std::fs::read_link(p).unwrap();
                out.insert(format!("{rel} -> {}", target.display()), vec![]);
            } else if meta.is_dir() {
                out.insert(format!("{rel}/"), vec![]);
                for child in std::fs::read_dir(p).unwrap() {
                    entry(base, &child.unwrap().path(), out);
                }
            } else {
                out.insert(rel, std::fs::read(p).unwrap());
            }
        }
        let mut out = BTreeMap::new();
        let apps = self.pm_dir.join(".apps");
        match std::fs::symlink_metadata(&apps) {
            Ok(_) => entry(&self.pm_dir, &apps, &mut out),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!(".apps metadata: {e}"),
        }
        let head = cadence_agent::reaper::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&self.pm_dir)
                .args(["rev-parse", "HEAD"]),
        )
        .unwrap();
        out.insert("<git HEAD>".into(), head.stdout);
        out
    }

    fn write(&self, rel: &str, bytes: &str) {
        let path = self.pm_dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }

    /// A list call must fail — hard (`rejected`) or retryable (`busy`
    /// after the read funnel's own retries) — and must not have created,
    /// removed or altered anything under `.apps`, and HEAD must not move.
    fn assert_refuses(&self, expected: &BTreeMap<String, Vec<u8>>, why: &str) {
        let err = match self.list() {
            Ok(v) => panic!("{why}: listed {v} instead of refusing"),
            Err(e) => e,
        };
        assert!(
            matches!(err.kind(), "rejected" | "busy"),
            "{why}: unexpected refusal kind {:?}: {err}",
            err.kind()
        );
        assert_eq!(
            &self.state(),
            expected,
            "{why}: refusal mutated tracker state"
        );
    }
}

/// A schema-valid unmigrated legacy installation record (`app::Record`)
/// under a project dir — enough for `legacy_records` to inventory it.
const LEGACY_RECORD: &str = "schema: 1\napp: old-app\nsource: {kind: path, path: /tmp/nowhere}\ninstalled_at: '2026-01-01T00:00:00Z'\ninstalled_by: operator\n";

#[test]
fn cad1318_fresh_registry_lists_empty_but_retained_or_damaged_state_refuses() {
    // -- Acceptance 1 + 2: a genuinely fresh workspace lists []; repeated
    // reads are read-only and create nothing. --------------------------
    let case = Case::fresh();
    let before = case.state();
    for round in 0..3 {
        let listed = case
            .list()
            .unwrap_or_else(|e| panic!("round {round}: fresh workspace must list [], got {e}"));
        assert_eq!(listed, json!([]), "fresh workspace must be an empty list");
    }
    assert_eq!(
        case.state(),
        before,
        "repeated reads created or moved registry state"
    );
    assert!(
        !case.pm_dir.join(".apps").exists(),
        "listing must not create .apps"
    );

    // An empty `.apps` scaffold (and an empty installations dir) is not
    // an installation artifact — a fresh workspace may carry it.
    std::fs::create_dir_all(case.pm_dir.join(".apps/installations")).unwrap();
    let scaffold = case.state();
    let listed = case
        .list()
        .expect("empty .apps scaffold must still list []");
    assert_eq!(listed, json!([]));
    assert_eq!(case.state(), scaffold, "scaffold read moved state");
    std::fs::remove_dir_all(case.pm_dir.join(".apps")).unwrap();

    // -- Acceptance 3: retained pending transactions refuse. -----------
    for marker in [
        ".apps/install-pending.yaml",
        ".apps/upgrade-pending.yaml",
        ".apps/pending.yaml",
    ] {
        let case = Case::fresh();
        case.write(marker, "0123456789abcdef0123456789abcdef");
        let state = case.state();
        case.assert_refuses(&state, marker);
    }

    // -- Acceptance 3: orphaned or damaged `.apps` contents refuse. ----
    // A stranded install journal.
    let case = Case::fresh();
    case.write(".apps/install-journals/deadbeef.yaml", "schema: 1\n");
    let state = case.state();
    case.assert_refuses(&state, "retained install journal");

    // An orphaned installation record with no catalog to account for it.
    let case = Case::fresh();
    case.write(
        ".apps/installations/0123456789abcdef0123456789abcdef/record.yaml",
        &LEGACY_RECORD.replace("old-app", "orphan"),
    );
    let state = case.state();
    case.assert_refuses(&state, "orphaned installation record");

    // Unexplained `.apps` content: a bare stray file is not scaffolding.
    let case = Case::fresh();
    case.write(".apps/stray.yaml", "x: 1\n");
    let state = case.state();
    case.assert_refuses(&state, "unexplained .apps content");

    // A symlinked `.apps` entry is an unsafe type, never an empty list.
    let case = Case::fresh();
    std::fs::create_dir_all(case.pm_dir.join(".apps")).unwrap();
    std::os::unix::fs::symlink("/tmp", case.pm_dir.join(".apps/link")).unwrap();
    let state = case.state();
    case.assert_refuses(&state, "symlinked .apps entry");

    // -- Acceptance 3: a malformed published catalog stays a refusal. --
    let case = Case::fresh();
    case.write(".apps/catalog.yaml", "schema: [broken\n");
    let state = case.state();
    case.assert_refuses(&state, "malformed catalog");

    // -- Acceptance 3: unmigrated legacy state refuses (never emptied).
    // The absent-catalog branch must surface the legacy rule's refusal —
    // an inventory read failure on the same state refuses too, so the
    // refusal is asserted, not a particular message.
    let case = Case::fresh();
    case.write("legacy/apps/old-app.yaml", LEGACY_RECORD);
    std::fs::create_dir_all(case.pm_dir.join("legacy/apps/old-app")).unwrap();
    let state = case.state();
    case.assert_refuses(&state, "unmigrated legacy installation");

    // A malformed legacy record — unreadable as a valid Record — also
    // refuses; damaged state is never reported as empty.
    let case = Case::fresh();
    case.write("legacy/apps/old-app.yaml", "schema: nope\n");
    std::fs::create_dir_all(case.pm_dir.join("legacy/apps/old-app")).unwrap();
    let state = case.state();
    case.assert_refuses(&state, "malformed legacy record");

    // -- Acceptance 3: lock/generation inconsistencies never become [].
    // An odd retained legacy generation is a crashed writer's seqlock
    // leftover with no live writer. With no catalog at all, the ticket
    // forbids declaring the registry empty: the real guard must refuse,
    // not answer `[]` through the crash-leftover locked read.
    let case = Case::fresh();
    std::fs::write(case.pm_dir.join(".git/cadence-legacy-generation"), "7").unwrap();
    let state = case.state();
    case.assert_refuses(&state, "odd retained legacy generation");

    // A malformed generation counter reads as unavailable — the same
    // inconsistency class. Still never an empty list.
    let case = Case::fresh();
    std::fs::write(
        case.pm_dir.join(".git/cadence-legacy-generation"),
        "not-a-number",
    )
    .unwrap();
    let state = case.state();
    case.assert_refuses(&state, "malformed legacy generation");
}
