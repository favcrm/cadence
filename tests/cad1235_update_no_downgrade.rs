//! Independent CAD-1235 refusal acceptance through the public update seam.
//! Synthetic ReleaseSource and isolated store; no GitHub or process effects.
use cadence_agent::rollout;
use cadence_agent::update::{self, Options, RestartOutcome, UpdateHost, Waiter};
use cadence_agent::upgrade::{
    self, ArtifactState, Current, Job, Layout, OnMain, ReleaseSource, Run,
};
use cadence_agent::{Error, Result};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CURRENT: &str = "1111111111111111111111111111111111111111";
const ANCESTOR: &str = "2222222222222222222222222222222222222222";
const DESCENDANT: &str = "3333333333333333333333333333333333333333";

#[derive(Clone, Copy)]
enum Selection {
    LatestApproved,
    Pinned,
}

struct SyntheticSource {
    target: &'static str,
    /// GitHub compare(base=target, head=current): `ahead` means current
    /// descends from target, so target is an ancestor/downgrade.
    ancestry: &'static str,
    schema: i64,
    downloads: Cell<usize>,
}

impl ReleaseSource for SyntheticSource {
    fn repo(&self) -> &str {
        "synthetic/cadence"
    }
    fn check_auth(&self) -> Result<()> {
        Ok(())
    }
    fn latest_green_main(&self) -> Result<Option<Run>> {
        Ok(Some(Run {
            id: 1,
            attempt: 1,
            head_sha: self.target.to_string(),
            head_branch: "main".into(),
            event: "push".into(),
            status: "completed".into(),
            conclusion: "success".into(),
        }))
    }
    fn on_main(&self, _: &str) -> Result<OnMain> {
        Ok(OnMain::Yes)
    }
    fn compare(&self, _: &str, _: &str) -> Result<Option<String>> {
        Ok(Some(self.ancestry.to_string()))
    }
    fn main_runs(&self, _: &str) -> Result<Vec<Run>> {
        Ok(Vec::new())
    }
    fn merge_group_runs(&self, _: &str) -> Result<Vec<Run>> {
        Ok(Vec::new())
    }
    fn jobs(&self, _: u64) -> Result<Vec<Job>> {
        Ok(Vec::new())
    }
    fn artifact(&self, _: u64, _: &str) -> Result<ArtifactState> {
        Ok(ArtifactState::Missing)
    }
    fn download(&self, _: u64, _: &str, _: &Path) -> Result<()> {
        self.downloads.set(self.downloads.get() + 1);
        Err(Error::internal("acceptance source must not download"))
    }
    fn verify_attestation(&self, _: &Path, _: &str) -> Result<String> {
        Err(Error::internal(
            "acceptance source must not verify artifacts",
        ))
    }
    fn merged_titles(&self, _: &str, _: &str) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn schema_version(&self, _: &str) -> Result<Option<i64>> {
        Ok(Some(self.schema))
    }
}

struct SyntheticHost {
    _dir: tempfile::TempDir,
    state: PathBuf,
    layout: Layout,
    source: SyntheticSource,
    pin: Option<&'static str>,
    pending_calls: Cell<usize>,
    drain_calls: Cell<usize>,
    restart_calls: Cell<usize>,
    start_board_calls: Cell<usize>,
}

impl SyntheticHost {
    fn new(
        selection: Selection,
        target: &'static str,
        ancestry: &'static str,
        schema: i64,
    ) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("cad1235-accept-")
            .tempdir_in("/tmp")
            .unwrap();
        let releases = dir.path().join("releases");
        let current_binary = releases.join(CURRENT).join("cadence");
        std::fs::create_dir_all(current_binary.parent().unwrap()).unwrap();
        std::fs::write(&current_binary, b"synthetic current release").unwrap();
        let link = dir.path().join("bin-cadence");
        std::os::unix::fs::symlink(&current_binary, &link).unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let db = rusqlite::Connection::open(rollout::db_file(&state)).unwrap();
        db.execute_batch(
            "CREATE TABLE schema_version (version INTEGER); INSERT INTO schema_version VALUES (33);",
        )
        .unwrap();
        drop(db);
        let pin = matches!(selection, Selection::Pinned).then_some(target);
        Self {
            _dir: dir,
            state,
            layout: Layout { releases, link },
            source: SyntheticSource {
                target,
                ancestry,
                schema,
                downloads: Cell::new(0),
            },
            pin,
            pending_calls: Cell::new(0),
            drain_calls: Cell::new(0),
            restart_calls: Cell::new(0),
            start_board_calls: Cell::new(0),
        }
    }
}

impl UpdateHost for SyntheticHost {
    fn state_dir(&self) -> &Path {
        &self.state
    }
    fn layout(&self) -> &Layout {
        &self.layout
    }
    fn source(&self) -> &dyn ReleaseSource {
        &self.source
    }
    fn identity(&self) -> &str {
        "operator:cad1235-acceptance"
    }
    fn progress(&self, _: &str) {}
    fn waiters(&self) -> Result<Vec<Waiter>> {
        Ok(Vec::new())
    }
    fn set_pending(&self, _: Option<&update::PendingUpdate>) -> Result<()> {
        self.pending_calls.set(self.pending_calls.get() + 1);
        Ok(())
    }
    fn pending(&self) -> Option<update::PendingUpdate> {
        None
    }
    fn set_drain(&self, _: bool) -> Result<()> {
        self.drain_calls.set(self.drain_calls.get() + 1);
        Ok(())
    }
    fn restart(&self, _: &Path) -> Result<RestartOutcome> {
        self.restart_calls.set(self.restart_calls.get() + 1);
        Ok(RestartOutcome::Clean)
    }
    fn daemon_build(&self) -> Result<Option<String>> {
        Ok(Some(CURRENT.to_string()))
    }
    fn board_build(&self) -> Result<Option<String>> {
        Ok(None)
    }
    fn board_running(&self) -> bool {
        false
    }
    fn start_board(&self, _: &Path) -> Result<()> {
        self.start_board_calls.set(self.start_board_calls.get() + 1);
        Ok(())
    }
    fn now(&self) -> f64 {
        1_800_000_000.0
    }
    fn sleep(&self, _: Duration) {}
    fn pin(&self) -> Option<&str> {
        self.pin
    }
}

fn assert_refused_before_effects(host: &SyntheticHost, target: &str, schema: i64) {
    let check = update::check(host).unwrap();
    let lines = check.lines().join("\n");
    assert!(
        lines.contains("blocked:"),
        "--check must report blocked: {lines}"
    );
    assert!(
        lines.contains(CURRENT) && lines.contains(target),
        "both SHAs must be named: {lines}"
    );
    assert!(
        lines.contains(&format!("schema {schema}")) && lines.contains("33"),
        "both schema values must be named: {lines}"
    );
    assert!(
        lines.contains("staging candidate") && lines.contains("--rollback"),
        "refusal must say how to proceed: {lines}"
    );
    assert!(
        !lines.contains("none recorded between"),
        "older target must not claim no changes: {lines}"
    );

    let err = update::run(host, &Options::default())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(target) && err.contains(CURRENT),
        "run refusal must identify both SHAs: {err}"
    );
    assert!(
        err.contains(&schema.to_string()) && err.contains("33"),
        "run refusal must identify both schemas: {err}"
    );
    assert!(
        err.contains("staging candidate") && err.contains("--rollback"),
        "run refusal must offer safe next steps: {err}"
    );
    assert!(
        matches!(upgrade::current(&host.layout).unwrap(), Current::Link { sha: Some(ref sha), .. } if sha == CURRENT),
        "running release link changed"
    );
    assert!(
        !host.layout.release_dir(target).exists(),
        "target release was installed"
    );
    assert_eq!(
        host.source.downloads.get(),
        0,
        "artifact download/install path ran"
    );
    assert_eq!(host.pending_calls.get(), 0, "update marker was written");
    assert_eq!(host.drain_calls.get(), 0, "daemon drain was changed");
    assert_eq!(host.restart_calls.get(), 0, "daemon restart ran");
    assert_eq!(host.start_board_calls.get(), 0, "board start ran");
    assert!(
        rollout::status(&host.state).unwrap()["held"].as_bool() != Some(true),
        "update lease was taken"
    );
    assert!(
        !host._dir.path().join("cadence-backups").exists(),
        "pre-update backup was taken"
    );
}

#[test]
fn ancestor_approved_target_is_refused_before_any_update_side_effect_for_both_selection_modes() {
    for selection in [Selection::LatestApproved, Selection::Pinned] {
        let host = SyntheticHost::new(selection, ANCESTOR, "ahead", 32);
        assert_refused_before_effects(&host, ANCESTOR, 32);
    }
}

#[test]
fn older_schema_target_is_refused_before_any_update_side_effect_for_both_selection_modes() {
    for selection in [Selection::LatestApproved, Selection::Pinned] {
        // The guard compares base=target with head=current: `behind`
        // means target descends from current, a valid forward relation.
        let host = SyntheticHost::new(selection, DESCENDANT, "behind", 32);
        assert_refused_before_effects(&host, DESCENDANT, 32);
    }
}

#[test]
fn schema_compatible_descendant_remains_available_in_both_selection_modes() {
    for selection in [Selection::LatestApproved, Selection::Pinned] {
        let host = SyntheticHost::new(selection, DESCENDANT, "behind", 33);
        let report = update::check(&host).unwrap();
        assert!(
            report.refusals.is_empty(),
            "valid descendant unexpectedly refused: {:?}",
            report.refusals
        );
        assert!(
            report
                .lines()
                .iter()
                .any(|line| line.starts_with("available: ")),
            "candidate was not offered: {:?}",
            report.lines()
        );
        assert!(
            !report
                .lines()
                .iter()
                .any(|line| line.starts_with("blocked:")),
            "valid candidate reported blocked: {:?}",
            report.lines()
        );
    }
}

#[test]
fn already_linked_older_schema_target_is_refused_before_any_update_side_effect_for_both_selection_modes(
) {
    for selection in [Selection::LatestApproved, Selection::Pinned] {
        let host = SyntheticHost::new(selection, CURRENT, "identical", 32);
        let linked_before = std::fs::read_link(&host.layout.link).unwrap();
        let binary_before = std::fs::read(&linked_before).unwrap();

        let report = update::check(&host).unwrap();
        let outcome = update::run(&host, &Options::default());
        assert!(
            outcome.is_err(),
            "an already-linked schema-32 release must refuse the schema-33 store"
        );
        assert!(
            report.up_to_date,
            "fixture must exercise an already-linked target"
        );
        assert_eq!(report.schema_current, Some(33));
        assert_eq!(report.schema_target, Some(32));
        assert!(
            !report.refusals.is_empty(),
            "check must refuse the already-linked older-schema target"
        );

        assert_eq!(
            std::fs::read_link(&host.layout.link).unwrap(),
            linked_before,
            "release link changed"
        );
        assert_eq!(
            std::fs::read(&linked_before).unwrap(),
            binary_before,
            "already-installed release was replaced or removed"
        );
        assert_eq!(
            host.source.downloads.get(),
            0,
            "artifact download/install path ran"
        );
        assert_eq!(host.pending_calls.get(), 0, "update marker was written");
        assert_eq!(host.drain_calls.get(), 0, "daemon drain was changed");
        assert_eq!(host.restart_calls.get(), 0, "daemon restart ran");
        assert_eq!(host.start_board_calls.get(), 0, "board start ran");
        assert!(
            !host._dir.path().join("cadence-backups").exists(),
            "pre-update backup was taken"
        );
        assert_eq!(rollout::store_schema(&host.state).unwrap(), Some(33));
        let db = rusqlite::Connection::open_with_flags(
            rollout::db_file(&host.state),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let lease_tables: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rollout_leases'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            lease_tables, 0,
            "lease acquisition must not run, even if it would later release the lease"
        );
    }
}
