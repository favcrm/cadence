//! CAD-511 / ADR 0007 T1 — the adversarial suite for the provision
//! verb and the doctor audit, all against `FixtureHost` (a tempdir
//! root + in-memory userdb): the real host is never touched.
//!
//! Every boundary test here is a kill test: it plants the violation
//! and asserts the check *fails*. If the guard were removed, nothing
//! would flag and the assertion would go red — which is the mutation
//! proof §13 requires.

// A test binary never runs the CAD-308 reaper, so its own spawns need
// not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;

use cadence_agent::agent_uid::audit::{self, Audit, Level};
use cadence_agent::agent_uid::fixture::FixtureHost;
use cadence_agent::agent_uid::provision::{self, Spec};
use cadence_agent::agent_uid::{
    enforce_root, AclEntry, Host, Meta, Principal, View, AGENT_HOME, AGENT_USER, HELPER_DEST,
    LANES_DIR, LAUNCH_GROUP, LIBEXEC_DIR, NOLOGIN, OPERATOR_USER, OPT_BIN, OPT_RELEASES, OPT_ROOT,
    REPOS_DIR, SHARED_GROUP, VAR_LIB,
};
use tempfile::TempDir;

// ---------- harness ----------

struct Lane {
    dir: TempDir,
    helper_src: PathBuf,
}

impl Lane {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let helper_src = dir.path().join("cadence-agent-exec");
        std::fs::write(&helper_src, b"fake-helper-bytes").unwrap();
        // A real build lands 0755 — the gate rejects group/other-writable
        // sources, so do not leave the file at the process umask (0664).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper_src, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Lane { dir, helper_src }
    }
    fn host(&self) -> FixtureHost {
        FixtureHost::new(self.dir.path())
    }
    fn spec(&self, dry_run: bool) -> Spec {
        Spec {
            operator: OPERATOR_USER.to_string(),
            helper: Some(self.helper_src.clone()),
            dry_run,
        }
    }
}

fn run_provision(host: &mut FixtureHost, lane: &Lane, dry_run: bool) -> provision::Report {
    let mut out = Vec::new();
    provision::run(host, &lane.spec(dry_run), &mut out).unwrap()
}

fn run_provision_text(
    host: &mut FixtureHost,
    lane: &Lane,
    dry_run: bool,
) -> (provision::Report, String) {
    let mut out = Vec::new();
    let report = provision::run(host, &lane.spec(dry_run), &mut out).unwrap();
    (report, String::from_utf8(out).unwrap())
}

fn audit(host: &FixtureHost) -> Audit {
    audit::audit(host, OPERATOR_USER)
}

fn row<'a>(audit: &'a Audit, name: &str) -> &'a audit::Row {
    audit
        .rows
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no row {name}"))
}

fn meta(host: &FixtureHost, path: &str) -> Meta {
    host.stat(path)
        .unwrap()
        .unwrap_or_else(|| panic!("no {path}"))
}

fn agent_uid(host: &FixtureHost) -> u32 {
    host.user(AGENT_USER).unwrap().unwrap().uid
}
fn gid(host: &FixtureHost, name: &str) -> u32 {
    host.group(name).unwrap().unwrap().gid
}

/// The fixture's full provision — asserts the run itself was clean.
fn provisioned(lane: &Lane) -> FixtureHost {
    let mut host = lane.host();
    let (report, text) = run_provision_text(&mut host, lane, false);
    assert!(
        !report.refused(),
        "provision refused on a clean fixture:\n{text}"
    );
    assert!(
        report.steps.iter().all(|s| s.applied || s.assess.ok()),
        "unapplied steps on a clean fixture:\n{text}"
    );
    host
}

/// A freshly provisioned host on its own lane. Each call gets a new
/// tempdir — reusing one lane for two runs would leave real dirs on
/// disk whose fs uid reads as a foreign owner to the second host.
/// Returns the lane so its TempDir outlives the host.
fn provisioned_fresh() -> (Lane, FixtureHost) {
    let lane = Lane::new();
    let host = provisioned(&lane);
    (lane, host)
}

// ---------- the root gate ----------

#[test]
fn provision_requires_real_root() {
    // `enforce_root` is the guard `cli()` calls before touching the
    // host — prove the guard itself (cli() runs against the live host,
    // which the tests must never mutate).
    assert!(enforce_root(0, 0).is_ok());
    assert!(enforce_root(1000, 1000).is_err(), "plain user passed");
    assert!(enforce_root(1000, 0).is_err(), "setuid non-root passed");
    assert!(
        enforce_root(0, 1000).is_err(),
        "real-root/effective-non-root must refuse — the argv gate checks both"
    );
    // And end-to-end on this test host: we are not root, so the verb
    // must refuse without touching anything.
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(
            provision::cli(false, PathBuf::from("/x/cadence-agent-exec"), OPERATOR_USER).unwrap(),
            2,
            "non-root caller was not refused"
        );
        assert_eq!(
            provision::cli(true, PathBuf::from("/x/cadence-agent-exec"), OPERATOR_USER).unwrap(),
            2,
            "even --dry-run is root-only"
        );
    }
}

// ---------- provision: the §5 shape ----------

#[test]
fn provision_creates_the_whole_5_layout() {
    let lane = Lane::new();
    let host = provisioned(&lane);

    let agent = host.user(AGENT_USER).unwrap().unwrap();
    assert_ne!(agent.uid, 0);
    assert_eq!(agent.home, AGENT_HOME);
    assert_eq!(agent.shell, NOLOGIN);
    assert_eq!(agent.locked, Some(true));

    for g in [AGENT_USER, SHARED_GROUP, LAUNCH_GROUP] {
        assert!(host.group(g).unwrap().is_some(), "missing group {g}");
    }
    let shared = host.group(SHARED_GROUP).unwrap().unwrap();
    assert!(shared.members.contains(OPERATOR_USER));
    assert!(shared.members.contains(AGENT_USER));
    let launch = host.group(LAUNCH_GROUP).unwrap().unwrap();
    assert!(launch.members.contains(OPERATOR_USER));
    assert!(
        !launch.members.contains(AGENT_USER),
        "the agent must never be in the launch edge — it would launch itself"
    );

    let (auid, agid) = (agent.uid, agent.gid);
    let (ouid, sgid, lgid) = (1000, gid(&host, SHARED_GROUP), gid(&host, LAUNCH_GROUP));
    let expect = [
        (AGENT_HOME, auid, agid, 0o750, true),
        (OPT_ROOT, 0, 0, 0o755, true),
        (LIBEXEC_DIR, 0, lgid, 0o750, true),
        (HELPER_DEST, 0, lgid, 0o4750, false),
        (VAR_LIB, ouid, sgid, 0o750, true),
        (LANES_DIR, ouid, sgid, 0o750, true),
        (REPOS_DIR, auid, sgid, 0o2750, true),
        (OPT_BIN, ouid, sgid, 0o755, true),
        (OPT_RELEASES, ouid, sgid, 0o755, true),
    ];
    for (path, uid, g, mode, is_dir) in expect {
        let m = meta(&host, path);
        assert_eq!(
            (m.uid, m.gid, m.mode, m.is_dir),
            (uid, g, mode, is_dir),
            "{path} metadata drifted"
        );
    }
    // The helper's bytes were installed, not just the metadata.
    assert_eq!(host.read_file(HELPER_DEST).unwrap(), b"fake-helper-bytes");
    // The repos default ACL grants the shared group rwX on new files.
    let acls = host.acls(REPOS_DIR).unwrap();
    assert!(
        acls.iter()
            .any(|e| e.default && e.tag == Principal::Group(sgid) && e.perms == 7),
        "missing default group ACL on repos: {acls:?}"
    );
}

#[test]
fn provision_is_idempotent() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    let report = run_provision(&mut host, &lane, false);
    assert!(
        report.clean(),
        "second run was not a no-op: {:?}",
        report
            .steps
            .iter()
            .map(|s| format!("{:?} → {:?}", s.action, s.assess))
            .collect::<Vec<_>>()
    );
    assert!(report.steps.iter().all(|s| !s.applied));
}

#[test]
fn dry_run_prints_every_action_and_changes_nothing() {
    let lane = Lane::new();
    let mut host = lane.host();
    let (report, text) = run_provision_text(&mut host, &lane, true);
    assert!(!report.steps.iter().any(|s| s.applied));
    // Nothing was created: no agent account, no dirs, no meta, no ACLs.
    assert!(host.user(AGENT_USER).unwrap().is_none());
    assert!(host.group(SHARED_GROUP).unwrap().is_none());
    for p in [AGENT_HOME, OPT_ROOT, VAR_LIB, HELPER_DEST] {
        assert!(host.stat(p).unwrap().is_none(), "{p} created by dry-run");
    }
    assert!(host.acl.is_empty());
    // And the transcript shows the whole §5 script.
    for line in [
        "groupadd --system cadence-agent",
        "groupadd cadence-launch",
        "useradd --system -m -d /home/cadence-agent -s /usr/sbin/nologin -g cadence-agent cadence-agent",
        "usermod -aG cadence-launch ubuntu",
        "install -d -o cadence-agent -g cadence-agent -m 0750 /home/cadence-agent",
        "install -o root -g cadence-launch -m 4750",
        "install -d -o ubuntu -g cadence -m 0750 /var/lib/cadence",
        "install -d -o cadence-agent -g cadence -m 2750 /var/lib/cadence/repos",
        "setfacl -m d:g:cadence:rwx /var/lib/cadence/repos",
        "dry-run: nothing was changed",
    ] {
        assert!(text.contains(line), "dry-run output missing {line:?}\n{text}");
    }
}

#[test]
fn provision_never_writes_under_operator_home_or_config() {
    let lane = Lane::new();
    let host = provisioned(&lane);
    // The plan's declared write set is confined to the §5 zones —
    // there is no action that *can* write under ~, /etc/sudoers*, or a
    // gitconfig path.
    for target in provision::plan_writes(&lane.spec(false)) {
        assert!(
            provision::ALLOWED_WRITE_ROOTS
                .iter()
                .any(|root| target == *root || target.starts_with(&format!("{root}/"))),
            "plan writes outside §5 zones: {target}"
        );
        assert!(!target.starts_with("/home/ubuntu"));
        assert!(!target.contains("sudoers"));
        assert!(!target.contains("gitconfig") && !target.contains("/.config/git"));
    }
    // Behavioural proof: walk the real tempdir — the only thing under
    // the operator home is what the fixture seeded (the dir itself).
    let (under_home, _) = host.walk("/home/ubuntu", 10_000).unwrap();
    assert!(
        under_home.is_empty(),
        "provision wrote under ~/: {under_home:?}"
    );
    assert!(!host.acl.contains_key("/home/ubuntu"));
    // uid-1000 git config untouched: no file materialised.
    for p in [
        "/home/ubuntu/.gitconfig",
        "/home/ubuntu/.config/git/config",
        "/etc/gitconfig",
        "/etc/sudoers",
        "/etc/sudoers.d/cadence",
    ] {
        assert!(host.stat(p).unwrap().is_none(), "{p} must not exist");
    }
}

#[test]
fn provision_repairs_drift_but_refuses_a_file() {
    let lane = Lane::new();
    // Looser modes and root/stray owners are repaired to §3.
    let mut host = lane.host();
    host.seed_dir(VAR_LIB, 0, 0, 0o777);
    host.seed_dir(REPOS_DIR, 1234, 1234, 0o700);
    run_provision(&mut host, &lane, false);
    assert_eq!(meta(&host, VAR_LIB).mode, 0o750);
    assert_eq!(meta(&host, VAR_LIB).uid, 1000);
    // Repos' stray uid/gid are reclaimed — the operator-tightened
    // permission bits stand (a repair intersects, never re-opens),
    // while the dropped setgid is restored: it is functional spec,
    // not a permission grant.
    let repos = meta(&host, REPOS_DIR);
    assert_eq!(
        (repos.uid, repos.mode),
        (agent_uid(&host), 0o2700),
        "operator-tightened perms loosened, or spec setgid lost"
    );

    // A regular file squatting on a §5 path is a refusal, not an
    // overwrite — on a fresh root (the lane above already has the
    // directory there).
    let lane2 = Lane::new();
    let mut host = lane2.host();
    host.seed_file(VAR_LIB, 0, 0, 0o644, b"squatter");
    let report = run_provision(&mut host, &lane2, false);
    assert!(report.refused(), "a file at {VAR_LIB} was provisioned over");
}

#[test]
fn provision_never_loosens_operator_hardening() {
    // Every harder-than-spec shape survives a second run.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.meta.get_mut(VAR_LIB).unwrap().mode = 0o700;
    host.meta.get_mut(REPOS_DIR).unwrap().mode = 0o2700;
    let report = run_provision(&mut host, &lane, false);
    assert_eq!(meta(&host, VAR_LIB).mode, 0o700, "0700 loosened to 0750");
    assert_eq!(meta(&host, REPOS_DIR).mode, 0o2700, "2700 loosened");
    // And the run is clean — tightened modes are not drift.
    assert!(
        report.clean(),
        "a tightened host was reported dirty: {:?}",
        report
            .steps
            .iter()
            .filter(|s| !s.assess.ok())
            .map(|s| format!("{:?} → {:?}", s.action, s.assess))
            .collect::<Vec<_>>()
    );
}

#[test]
fn provision_refuses_incomparable_mode_and_foreign_owner() {
    let lane = Lane::new();
    // 0705 vs spec 0750: grants other r-x while dropping group r-x —
    // neither tighter nor looser, so provision must not guess.
    let mut host = lane.host();
    host.seed_dir(VAR_LIB, 0, 0, 0o705);
    let report = run_provision(&mut host, &lane, false);
    assert!(report.refused(), "incomparable mode 0705 was rewritten");

    // A foreign account's tree is never chowned into the spec.
    let lane = Lane::new();
    let mut host = lane.host();
    host.add_user_record("mallory", 1500, 1500, "/home/mallory", "/bin/bash");
    host.seed_dir(VAR_LIB, 1500, 1500, 0o750);
    let report = run_provision(&mut host, &lane, false);
    assert!(
        report.refused(),
        "a directory owned by mallory was chowned into the boundary"
    );
}

#[test]
fn provision_refuses_a_uid0_agent() {
    let lane = Lane::new();
    let mut host = lane.host();
    // `cadence-agent` already exists — as uid 0. Blessing it would
    // install a setuid bridge straight into root's own uid.
    host.add_user_record(AGENT_USER, 0, 0, "/root", "/bin/bash");
    let report = run_provision(&mut host, &lane, false);
    assert!(report.refused(), "a uid-0 agent account was blessed");
    // The fs phase never ran.
    assert!(host.stat(VAR_LIB).unwrap().is_none());
    assert!(host.stat(OPT_ROOT).unwrap().is_none());
}

#[test]
fn preflight_refuses_when_git_config_is_prearmed() {
    let lane = Lane::new();
    let mut host = lane.host();
    host.write_file("/home/ubuntu/.gitconfig", "[safe]\n\tdirectory = *\n");
    let (report, text) = run_provision_text(&mut host, &lane, false);
    assert!(report.refused());
    assert!(!report.preflight.is_empty());
    assert!(text.contains("refused:"), "{text}");
    // Account phase ran (it must — the uid has to resolve to test the
    // boundary), the fs phase did not.
    assert!(host.user(AGENT_USER).unwrap().is_some());
    assert!(host.stat(VAR_LIB).unwrap().is_none());
}

#[test]
fn preflight_refuses_on_an_agent_reachable_home_acl() {
    let lane = Lane::new();
    let mut host = lane.host();
    // The walk only visits real paths — create the dir the ACL rides
    // on, then grant the agent's group (gid 900: the fixture's next
    // allocation — the agent's primary group, created in the account
    // phase before pre-flight runs).
    host.seed_dir("/home/ubuntu/.ssh", 1000, 1000, 0o700);
    host.seed_acl(
        "/home/ubuntu/.ssh",
        AclEntry {
            default: false,
            tag: Principal::Group(900),
            perms: 5,
        },
    );
    let report = run_provision(&mut host, &lane, false);
    assert!(
        report.refused(),
        "an ACL under ~ granting the agent's group did not refuse provision"
    );
}

#[test]
fn provision_without_helper_source_leaves_it_absent() {
    let lane = Lane::new();
    let mut host = lane.host();
    let mut spec = lane.spec(false);
    spec.helper = None;
    let mut out = Vec::new();
    let report = provision::run(&mut host, &spec, &mut out).unwrap();
    // The install step fails loudly — and the failure must surface in
    // the report (a caller maps it to exit 2).
    assert!(host.stat(HELPER_DEST).unwrap().is_none());
    assert!(!report.clean());
    assert!(
        report.refused(),
        "a failed apply must mark the report refused — exit 2, not 0"
    );
    assert!(report.failed);
}

// ---------- C1: a planted symlink refuses every verb ----------

#[test]
fn symlink_at_a_spec_path_refuses_assess_and_apply() {
    for path in [
        VAR_LIB,
        LANES_DIR,
        REPOS_DIR,
        OPT_ROOT,
        LIBEXEC_DIR,
        AGENT_HOME,
    ] {
        let lane = Lane::new();
        let mut host = lane.host();
        host.seed_symlink(path, "/tmp").unwrap();
        let report = run_provision(&mut host, &lane, false);
        assert!(
            report.refused(),
            "a symlink planted at {path} did not refuse provision"
        );
        // The link is still a link — nothing followed or overwrote it.
        let m = meta(&host, path);
        assert!(m.is_symlink, "{path}: symlink was overwritten");
    }
    // The helper's dest: a symlink refuses the install.
    let lane = Lane::new();
    let mut host = lane.host();
    host.seed_symlink(HELPER_DEST, "/etc/passwd").unwrap();
    let report = run_provision(&mut host, &lane, false);
    assert!(report.refused(), "installing over a symlink passed");
    assert_eq!(
        std::fs::read_link(host.root.join(HELPER_DEST.trim_start_matches('/'))).unwrap(),
        PathBuf::from("/etc/passwd"),
        "the symlink target was disturbed"
    );
}

#[test]
fn apply_verbs_refuse_a_planted_link_even_mid_race() {
    // assess→apply is a race window; the verbs themselves must refuse.
    let lane = Lane::new();
    let mut host = lane.host();
    host.seed_dir(VAR_LIB, 1000, 1000, 0o750);
    host.seed_symlink(LANES_DIR, "/etc").unwrap();
    assert!(
        host.mkdir(LANES_DIR).is_err(),
        "mkdir followed a symlinked final component"
    );
    assert!(
        host.set_meta(LANES_DIR, OPERATOR_USER, SHARED_GROUP, 0o750)
            .is_err(),
        "set_meta followed a symlink"
    );
    // A symlinked intermediate: /var/lib/cadence → elsewhere.
    let lane2 = Lane::new();
    let mut host = lane2.host();
    host.seed_symlink(VAR_LIB, "/tmp").unwrap();
    assert!(
        host.mkdir(LANES_DIR).is_err(),
        "mkdir descended through a symlinked intermediate"
    );
    assert!(
        host.set_meta(LANES_DIR, OPERATOR_USER, SHARED_GROUP, 0o750)
            .is_err(),
        "set_meta descended through a symlinked intermediate"
    );
    // An untrusted chain owner refuses too — an agent-owned
    // intermediate means the agent controls a prefix of the path.
    let lane3 = Lane::new();
    let mut host = lane3.host();
    host.seed_dir(VAR_LIB, 4242, 4242, 0o750);
    assert!(
        host.mkdir(LANES_DIR).is_err(),
        "mkdir descended into an untrusted-owner directory"
    );
}

#[test]
fn reprovision_refuses_when_a_path_became_a_symlink() {
    // The assess-time symlink verdict: provision a clean host, swap a
    // §5 dir for a link, re-run — refusal, and the link survives.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    let phys = host.root.join(LANES_DIR.trim_start_matches('/'));
    std::fs::remove_dir(&phys).unwrap();
    host.meta.remove(LANES_DIR);
    host.seed_symlink(LANES_DIR, "/tmp").unwrap();
    let report = run_provision(&mut host, &lane, false);
    assert!(report.refused());
    assert!(meta(&host, LANES_DIR).is_symlink);
}

// ---------- C2: the helper source is vetted, never discovered ----------

#[test]
fn helper_source_refuses_agent_owned_group_writable_and_symlink() {
    // Agent-owned source.
    let lane = Lane::new();
    let mut host = lane.host();
    let auid = 4242;
    host.meta.insert(
        lane.helper_src.display().to_string(),
        Meta {
            uid: auid,
            gid: 1000,
            mode: 0o755,
            is_dir: false,
            is_file: true,
            is_symlink: false,
        },
    );
    assert!(
        host.helper_source(&lane.helper_src).is_err(),
        "an agent-owned helper source passed the gate"
    );
    assert!(
        run_provision(&mut host, &lane, false).refused(),
        "an agent-owned helper was installed"
    );

    // Group/other-writable source.
    let lane = Lane::new();
    let mut host = lane.host();
    host.meta.insert(
        lane.helper_src.display().to_string(),
        Meta {
            uid: 1000,
            gid: 1000,
            mode: 0o664,
            is_dir: false,
            is_file: true,
            is_symlink: false,
        },
    );
    assert!(
        host.helper_source(&lane.helper_src).is_err(),
        "a group-writable helper source passed the gate"
    );

    // A symlink source — O_NOFOLLOW refuses at open.
    let lane = Lane::new();
    let host = lane.host();
    let link = lane.dir.path().join("link-helper");
    std::os::unix::fs::symlink(&lane.helper_src, &link).unwrap();
    assert!(
        host.helper_source(&link).is_err(),
        "a symlinked helper source passed the gate"
    );

    // A fifo source — O_NONBLOCK means the open cannot hang either.
    let lane = Lane::new();
    let mut host = lane.host();
    let fifo = lane.dir.path().join("fifo-helper");
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
    assert!(
        host.helper_source(&fifo).is_err(),
        "a fifo helper source passed the gate"
    );

    // A relative path is refused outright — under sudo the cwd is
    // untrusted.
    assert!(host
        .install(&PathBuf::from("rel"), "/x", "root", "root", 0o755)
        .is_err());
}

#[test]
fn installed_helper_bytes_match_the_verified_source() {
    let lane = Lane::new();
    let host = provisioned(&lane);
    // The dest holds exactly the vetted source bytes — same hash the
    // vet+hash one-fd gate produced.
    assert_eq!(
        host.helper_source(&lane.helper_src).unwrap(),
        host.file_sha256(HELPER_DEST).unwrap()
    );
    // A rebuilt source re-installs — the byte compare drives it.
    let mut host = host;
    std::fs::write(&lane.helper_src, b"rebuilt-helper").unwrap();
    let report = run_provision(&mut host, &lane, false);
    assert!(!report.clean(), "a rebuilt helper was not reinstalled");
    assert_eq!(host.read_file(HELPER_DEST).unwrap(), b"rebuilt-helper");
}

// ---------- account-shape refusals ----------

#[test]
fn provision_refuses_agent_sharing_operator_uid() {
    let lane = Lane::new();
    let mut host = lane.host();
    // The agent account answering to uid 1000 is no boundary.
    host.add_user_record(AGENT_USER, 1000, 900, AGENT_HOME, NOLOGIN);
    let report = run_provision(&mut host, &lane, false);
    assert!(
        report.refused(),
        "an agent sharing the operator uid provisioned"
    );
}

#[test]
fn audit_fails_when_agent_shares_operator_uid() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    let agid = host.user(AGENT_USER).unwrap().unwrap().gid;
    host.add_user_record(AGENT_USER, 1000, agid, AGENT_HOME, NOLOGIN);
    assert_eq!(row(&audit(&host), "agent-user").level, Level::Fail);
}

#[test]
fn audit_fails_when_operator_leaves_launch_group() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.groups
        .get_mut(LAUNCH_GROUP)
        .unwrap()
        .members
        .remove(OPERATOR_USER);
    assert_eq!(
        row(&audit(&host), "agent-groups").level,
        Level::Fail,
        "operator outside cadence-launch passed — the helper's gate edge"
    );
}

#[test]
fn apply_failure_on_the_last_step_refuses_the_run() {
    // The failure must survive as non-zero even when it is the final
    // action — no trailing step exists to carry the bad news.
    // OPT_RELEASES is the plan's last action: inject the apply
    // failure there — assess must still pass, then apply must fail
    // and the report must carry it to the exit status.
    let lane = Lane::new();
    let mut host = lane.host();
    host.fail_ops.insert(OPT_RELEASES.to_string());
    let report = run_provision(&mut host, &lane, false);
    assert!(report.failed, "the failed apply was not recorded");
    assert!(report.refused(), "a failed apply exited clean");
}

// ---------- audit: artifact checks ----------

#[test]
fn audit_clean_on_a_provisioned_host() {
    let lane = Lane::new();
    let host = provisioned(&lane);
    let report = audit(&host);
    assert!(report.provisioned);
    assert_eq!(
        report.level(),
        Level::Ok,
        "rows: {:?}",
        report
            .rows
            .iter()
            .map(|r| format!("{}={}", r.name, r.level.as_str()))
            .collect::<Vec<_>>()
    );
}

#[test]
fn audit_warns_not_fails_on_a_bare_host() {
    let lane = Lane::new();
    let host = lane.host();
    let report = audit(&host);
    assert!(!report.provisioned);
    // Artifacts absent → warn; the two negatives are clean → ok.
    for r in &report.rows {
        let want = if r.negative { Level::Ok } else { Level::Warn };
        assert_eq!(r.level, want, "row {} ({:?})", r.name, r.detail);
    }
    assert_eq!(report.level(), Level::Warn);
}

#[test]
fn audit_fails_on_each_mutated_artifact() {
    // Agent account: uid 0.
    let (_lane, mut host) = provisioned_fresh();
    host.add_user_record(AGENT_USER, 0, gid(&host, AGENT_USER), AGENT_HOME, NOLOGIN);
    let report = audit(&host);
    assert_eq!(
        row(&report, "agent-user").level,
        Level::Fail,
        "uid-0 agent passed"
    );

    // Agent account: a shell that logs in.
    let (_lane, mut host) = provisioned_fresh();
    let u = host.user(AGENT_USER).unwrap().unwrap();
    host.add_user_record(AGENT_USER, u.uid, u.gid, AGENT_HOME, "/bin/bash");
    assert_eq!(row(&audit(&host), "agent-user").level, Level::Fail);

    // A stray member in the launch edge — `nobody` could now exec as
    // the agent.
    let (_lane, mut host) = provisioned_fresh();
    host.groups
        .get_mut(LAUNCH_GROUP)
        .unwrap()
        .members
        .insert("nobody".to_string());
    assert_eq!(row(&audit(&host), "agent-groups").level, Level::Fail);

    // The agent inside the operator's own group — a traversal grant
    // into ~ wearing a different name.
    let (_lane, mut host) = provisioned_fresh();
    host.groups
        .get_mut(OPERATOR_USER)
        .unwrap()
        .members
        .insert(AGENT_USER.to_string());
    assert_eq!(row(&audit(&host), "agent-groups").level, Level::Fail);

    // Every fs artifact: flip each mode in turn.
    for (path, bad_mode, row_name) in [
        (AGENT_HOME, 0o755, "agent-home"),
        (OPT_ROOT, 0o777, "opt-tree"),
        (LIBEXEC_DIR, 0o755, "opt-tree"),
        (VAR_LIB, 0o755, "share-tree"),
        (REPOS_DIR, 0o750, "share-tree"), // setgid dropped
        (OPT_BIN, 0o757, "opt-tree"),
    ] {
        let (_lane, mut host) = provisioned_fresh();
        host.meta.get_mut(path).unwrap().mode = bad_mode;
        let report = audit(&host);
        assert_eq!(
            row(&report, row_name).level,
            Level::Fail,
            "{path} mode {:04o} was not caught",
            bad_mode
        );
    }

    // The helper: dropped setuid, wrong owner, absent — each fails.
    let (_lane, mut host) = provisioned_fresh();
    host.meta.get_mut(HELPER_DEST).unwrap().mode = 0o750;
    assert_eq!(row(&audit(&host), "helper").level, Level::Fail);
    let (_lane, mut host) = provisioned_fresh();
    host.meta.get_mut(HELPER_DEST).unwrap().uid = 1000;
    assert_eq!(row(&audit(&host), "helper").level, Level::Fail);
    let (_lane, mut host) = provisioned_fresh();
    host.meta.remove(HELPER_DEST);
    std::fs::remove_file(host.root.join(HELPER_DEST.trim_start_matches('/'))).unwrap();
    assert_eq!(row(&audit(&host), "helper").level, Level::Fail);

    // The repos default ACL removed — new checkouts would be
    // operator-unreachable.
    let (_lane, mut host) = provisioned_fresh();
    host.acl.get_mut(REPOS_DIR).unwrap().clear();
    assert_eq!(row(&audit(&host), "share-tree").level, Level::Fail);
}

#[test]
fn audit_fails_when_a_piece_is_missing() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.users.remove(AGENT_USER);
    // A half-provisioned host is fail, not warn.
    let report = audit(&host);
    assert!(report.provisioned);
    assert_eq!(row(&report, "agent-user").level, Level::Fail);
}

// ---------- audit: the home-ACL negative ----------

#[test]
fn audit_flags_an_acl_grant_under_operator_home() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    let auid = agent_uid(&host);
    // A subtree under ~ the agent uid can read — e.g. someone ran
    // `setfacl -m u:cadence-agent:rx ~/.ssh` to debug something once.
    host.write_file("/home/ubuntu/.ssh/config", "Host *\n");
    host.seed_acl(
        "/home/ubuntu/.ssh",
        AclEntry {
            default: false,
            tag: Principal::User(auid),
            perms: 5,
        },
    );
    let report = audit(&host);
    let r = row(&report, "home-acl");
    assert_eq!(
        r.level,
        Level::Fail,
        "ACL grant under ~ passed: {:?}",
        r.value
    );
    assert!(r.value["findings"]
        .as_str()
        .map(|s| s.contains(".ssh"))
        .unwrap_or_else(|| r.value["findings"].to_string().contains(".ssh")));
}

#[test]
fn audit_flags_an_acl_grant_by_group_and_a_default_acl() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    let sgid = gid(&host, SHARED_GROUP);
    // Shared-group membership makes ANY `g:cadence` grant under ~ a
    // traversal path — including a *default* ACL that would keep
    // re-applying itself to new files.
    host.write_file("/home/ubuntu/pm/issue.md", "x\n");
    host.seed_acl(
        "/home/ubuntu/pm",
        AclEntry {
            default: true,
            tag: Principal::Group(sgid),
            perms: 7,
        },
    );
    let report = audit(&host);
    assert_eq!(row(&report, "home-acl").level, Level::Fail);
}

#[test]
fn audit_flags_a_world_traversable_home() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.meta.get_mut("/home/ubuntu").unwrap().mode = 0o755;
    let report = audit(&host);
    assert_eq!(row(&report, "home-acl").level, Level::Fail);
}

#[test]
fn audit_ignores_acls_for_unrelated_principals() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    // An ACL for some other uid/group under ~ is legal — only the
    // agent domain is asserted against.
    host.write_file("/home/ubuntu/shared/doc", "x\n");
    host.seed_acl(
        "/home/ubuntu/shared",
        AclEntry {
            default: false,
            tag: Principal::User(4242),
            perms: 5,
        },
    );
    host.seed_acl(
        "/home/ubuntu/shared",
        AclEntry {
            default: false,
            tag: Principal::Group(4242),
            perms: 5,
        },
    );
    let report = audit(&host);
    assert_eq!(
        row(&report, "home-acl").level,
        Level::Ok,
        "unrelated ACLs must not trip the negative: {:?}",
        row(&report, "home-acl").value
    );
}

// ---------- audit: the git-config negative ----------

#[test]
fn audit_flags_safe_directory_covering_the_store() {
    for (label, cfg) in [
        ("exact", "[safe]\n\tdirectory = /var/lib/cadence\n"),
        ("inside", "[safe]\n\tdirectory = /var/lib/cadence/repos/x\n"),
        ("star", "[safe]\n\tdirectory = *\n"),
        (
            "tilde-norm",
            "[safe]\n\tdirectory = /var/lib/../lib/cadence\n",
        ),
        ("trailing", "[safe]\n\tdirectory = /var/lib/cadence/\n"),
        // git's `dir/*` form — every path under dir is covered, so a
        // wildcard rooted above the store arms every checkout in it.
        ("var-star", "[safe]\n\tdirectory = /var/*\n"),
        ("root-star", "[safe]\n\tdirectory = /*\n"),
        ("lib-star", "[safe]\n\tdirectory = /var/lib/*\n"),
        ("store-star", "[safe]\n\tdirectory = /var/lib/cadence/*\n"),
        ("tilde-star", "[safe]\n\tdirectory = ~/*\n"),
    ] {
        let lane = Lane::new();
        let mut host = provisioned(&lane);
        host.write_file("/home/ubuntu/.gitconfig", cfg);
        let report = audit(&host);
        if label == "tilde-star" {
            assert_ne!(
                row(&report, "git-config").level,
                Level::Fail,
                "~/covers the operator's home only — it must not flag"
            );
            continue;
        }
        assert_eq!(
            row(&report, "git-config").level,
            Level::Fail,
            "{label}: {cfg:?} was not flagged"
        );
    }
    // And the non-covering neighbours stay quiet.
    for cfg in [
        "[safe]\n\tdirectory = /var/lib/cadencesnap\n",
        "[safe]\n\tdirectory = /home/ubuntu/*\n",
        "[safe]\n\tdirectory = /opt/*\n",
    ] {
        let lane = Lane::new();
        let mut host = provisioned(&lane);
        host.write_file("/home/ubuntu/.gitconfig", cfg);
        assert_eq!(
            row(&audit(&host), "git-config").level,
            Level::Ok,
            "{cfg:?} was flagged but covers nothing"
        );
    }
}

#[test]
fn audit_flags_exec_capable_keys_reaching_the_store() {
    // A command-channel value inside the agent domain — the operator's
    // own config writing a hook into agent-writable ground.
    for (label, cfg) in [
        ("fsmonitor", "[core]\n\tfsmonitor = /var/lib/cadence/spy\n"),
        (
            "hooksPath",
            "[core]\n\thooksPath = /var/lib/cadence/hooks\n",
        ),
        ("pager", "[core]\n\tpager = /var/lib/cadence/less\n"),
        (
            "sshCommand",
            "[core]\n\tsshCommand = /var/lib/cadence/ssh\n",
        ),
        (
            "cred-helper",
            "[credential]\n\thelper = /var/lib/cadence/steal\n",
        ),
        (
            "cred-url",
            "[credential \"https://x\"]\n\thelper = /var/lib/cadence/steal\n",
        ),
        ("filter", "[filter \"x\"]\n\tclean = /var/lib/cadence/f\n"),
    ] {
        let lane = Lane::new();
        let mut host = provisioned(&lane);
        host.write_file("/home/ubuntu/.gitconfig", cfg);
        assert_eq!(
            row(&audit(&host), "git-config").level,
            Level::Fail,
            "{label}: {cfg:?} was not flagged"
        );
    }
    // A config file already inside the store is agent-written: an
    // exec key there flags on its own, whatever value it carries.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[include]\n\tpath = /var/lib/cadence/repos/x/.gitconfig\n",
    );
    assert_eq!(row(&audit(&host), "git-config").level, Level::Fail);
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file("/home/ubuntu/.gitconfig", "[include]\n\tpath = ~/x.inc\n");
    host.write_file(
        "/home/ubuntu/x.inc",
        "[include]\n\tpath = /var/lib/cadence/in.inc\n",
    );
    host.write_file("/var/lib/cadence/in.inc", "[core]\n\tpager = less\n");
    let report = audit(&host);
    assert_eq!(
        row(&report, "git-config").level,
        Level::Fail,
        "an exec-capable key in agent-domain config was not flagged"
    );
}

#[test]
fn audit_flags_include_path_into_the_store() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[include]\n\tpath = /var/lib/cadence/repos/x/.gitconfig\n",
    );
    let report = audit(&host);
    assert_eq!(row(&report, "git-config").level, Level::Fail);
}

#[test]
fn audit_follows_include_chains() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    // The violation lives one hop away — ~/.gitconfig includes a file
    // that carries the armed safe.directory.
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[include]\n\tpath = ~/extra.inc\n",
    );
    host.write_file(
        "/home/ubuntu/extra.inc",
        "[safe]\n\tdirectory = /var/lib/cadence\n",
    );
    let report = audit(&host);
    assert_eq!(
        row(&report, "git-config").level,
        Level::Fail,
        "a violation behind include.path was not followed"
    );
}

#[test]
fn audit_flags_env_carried_config() {
    // GIT_CONFIG_COUNT pairs.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.env.insert("GIT_CONFIG_COUNT".into(), "1".into());
    host.env
        .insert("GIT_CONFIG_KEY_0".into(), "safe.directory".into());
    host.env
        .insert("GIT_CONFIG_VALUE_0".into(), "/var/lib/cadence".into());
    let report = audit(&host);
    assert_eq!(row(&report, "git-config").level, Level::Fail);

    // GIT_CONFIG_PARAMETERS squote pairs.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.env.insert(
        "GIT_CONFIG_PARAMETERS".into(),
        "'safe.directory'='/var/lib/cadence'".into(),
    );
    let report = audit(&host);
    assert_eq!(row(&report, "git-config").level, Level::Fail);

    // GIT_CONFIG_GLOBAL itself pointing inside the store loads
    // agent-written config — flag the file's contents and the fact it
    // was read from the agent domain at all.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.env.insert(
        "GIT_CONFIG_GLOBAL".into(),
        "/var/lib/cadence/repos/x/.gitconfig".into(),
    );
    host.write_file(
        "/var/lib/cadence/repos/x/.gitconfig",
        "[include]\n\tpath = /var/lib/cadence/other\n",
    );
    let report = audit(&host);
    assert_eq!(row(&report, "git-config").level, Level::Fail);
}

#[test]
fn audit_ignores_innocent_git_config() {
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[user]\n\tname = Op\n[safe]\n\tdirectory = /home/ubuntu/Project/cadence\n\
         [includeIf \"gitdir:~/work/\"]\n\tpath = ~/work.inc\n",
    );
    host.write_file("/home/ubuntu/work.inc", "[user]\n\temail = o@x\n");
    let report = audit(&host);
    assert_eq!(
        row(&report, "git-config").level,
        Level::Ok,
        "innocent config flagged: {:?}",
        row(&report, "git-config").value
    );
}

#[test]
fn audit_flags_chgrp_and_mode_grants_under_home() {
    // A chgrp into the agent's group with group bits on — no ACL,
    // just the mode — reaches the agent domain.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    let sgid = gid(&host, SHARED_GROUP);
    host.seed_file("/home/ubuntu/leak", 1000, sgid, 0o640, b"x\n");
    assert_eq!(
        row(&audit(&host), "home-acl").level,
        Level::Fail,
        "a chgrp'ed file under ~ was not flagged"
    );

    // A file owned by the agent uid itself.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.seed_file(
        "/home/ubuntu/planted",
        agent_uid(&host),
        1000,
        0o600,
        b"x\n",
    );
    assert_eq!(
        row(&audit(&host), "home-acl").level,
        Level::Fail,
        "an agent-owned file under ~ was not flagged"
    );

    // World-writable under ~ — anyone, the agent included, writes.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.seed_dir("/home/ubuntu/pub", 1000, 1000, 0o777);
    assert_eq!(
        row(&audit(&host), "home-acl").level,
        Level::Fail,
        "a world-writable dir under ~ was not flagged"
    );
}

#[test]
fn audit_sees_grants_through_agent_supplementary_groups() {
    // `usermod -aG docker cadence-agent` makes docker-gid files
    // agent-reachable — getgrouplist must fold it into the sweep.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.groups.insert(
        "docker".to_string(),
        cadence_agent::agent_uid::Group {
            name: "docker".into(),
            gid: 4242,
            members: std::collections::BTreeSet::from([AGENT_USER.to_string()]),
        },
    );
    host.seed_file("/home/ubuntu/leak2", 1000, 4242, 0o640, b"x\n");
    assert_eq!(
        row(&audit(&host), "home-acl").level,
        Level::Fail,
        "a grant through the agent's supplementary group was not flagged"
    );
}

#[test]
fn include_targets_are_vetted_and_fail_closed() {
    use std::os::unix::ffi::OsStrExt;
    // A fifo include target cannot be parsed — the audit cannot hang
    // on it (O_NONBLOCK refuses the open), and a config source it
    // cannot verify fails closed rather than claiming ok.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[include]\n\tpath = ~/fifo.inc\n",
    );
    let fifo = host.root.join("home/ubuntu/fifo.inc");
    let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
    let report = audit(&host); // returns — no hang
    assert_eq!(
        row(&report, "git-config").level,
        Level::Fail,
        "an unverifiable include must fail closed, not pass"
    );

    // A symlinked include: every hop is resolved (link text only, the
    // link is never opened) and where it *lands* is judged — git would
    // follow it into the agent domain, so a store-resolving link flags.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[include]\n\tpath = ~/link.inc\n",
    );
    let real = lane.dir.path().join("home/ubuntu/link.inc");
    std::os::unix::fs::symlink("/var/lib/cadence/evil", &real).unwrap();
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "a symlinked include into the store was not flagged"
    );
    // …while a link staying under ~ is inert.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file("/home/ubuntu/.gitconfig", "[include]\n\tpath = ~/ok.inc\n");
    host.write_file("/home/ubuntu/real.inc", "[user]\n\tname = o\n");
    std::os::unix::fs::symlink(
        "/home/ubuntu/real.inc",
        lane.dir.path().join("home/ubuntu/ok.inc"),
    )
    .unwrap();
    assert_ne!(row(&audit(&host), "git-config").level, Level::Fail);

    // An over-cap include target is refused, not OOM'd — and the
    // piece the audit could not check fails closed even when the
    // violation sits past the bound.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file("/home/ubuntu/.gitconfig", "[include]\n\tpath = ~/big.inc\n");
    let mut big = vec![b' '; 1 << 21];
    big.extend_from_slice(b"\n[safe]\n\tdirectory = *\n");
    std::fs::write(lane.dir.path().join("home/ubuntu/big.inc"), &big).unwrap();
    let report = audit(&host);
    assert_eq!(
        row(&report, "git-config").level,
        Level::Fail,
        "an over-cap config source is unverifiable — it must fail closed, not pass"
    );
}

#[test]
fn audit_parses_config_with_git_not_a_line_scanner() {
    // `[safe] directory = *` on one line — a text parser reads only
    // the header; git's grammar applies the assignment. Verified
    // against git 2.43: it lists `safe.directory=*`.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file("/home/ubuntu/.gitconfig", "[safe] directory = *\n");
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "same-line `[safe] directory = *` was not flagged"
    );

    // A quoted value may continue across lines with a trailing `\`
    // inside the quotes — git joins it to `/var/lib/cadence`.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[safe]\n\tdirectory = \"/var/lib/cad\\\nence\"\n",
    );
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "quoted backslash-continuation to /var/lib/cadence was not flagged"
    );

    // A malformed file is not silently clean — git refuses it, so the
    // piece is unverifiable and fails closed.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file("/home/ubuntu/.gitconfig", "this is not {{{ a config\n");
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "an unparseable operator config must fail closed"
    );
}

#[test]
fn audit_flags_bang_aliases_and_the_wider_exec_key_set() {
    // `alias.st = !…` runs its payload through `sh -c` on `git st` —
    // a shell line cannot be path-bounded, so it flags wherever set.
    for (label, cfg) in [
        ("alias-store", "[alias]\n\tco = !/var/lib/cadence/x.sh\n"),
        ("alias-any", "[alias]\n\tst = !echo pwned\n"),
        ("instaweb", "[instaweb]\n\thttpd = /var/lib/cadence/httpd\n"),
        ("man-viewer", "[man]\n\tviewer = /var/lib/cadence/v\n"),
        ("man-cmd", "[man \"x\"]\n\tcmd = /var/lib/cadence/v\n"),
        (
            "sendmailcmd",
            "[sendemail]\n\tsendmailcmd = /var/lib/cadence/sendmail\n",
        ),
        ("web-browser", "[web]\n\tbrowser = /var/lib/cadence/b\n"),
        (
            "browser-path",
            "[browser \"x\"]\n\tpath = /var/lib/cadence/b\n",
        ),
        (
            "difftool-path",
            "[difftool \"x\"]\n\tpath = /var/lib/cadence/d\n",
        ),
        (
            "mergetool-path",
            "[mergetool \"x\"]\n\tpath = /var/lib/cadence/m\n",
        ),
        ("ssh-variant", "[ssh]\n\tvariant = /var/lib/cadence/ssh\n"),
    ] {
        let lane = Lane::new();
        let mut host = provisioned(&lane);
        host.write_file("/home/ubuntu/.gitconfig", cfg);
        assert_eq!(
            row(&audit(&host), "git-config").level,
            Level::Fail,
            "{label}: {cfg:?} was not flagged"
        );
    }
    // A `!`-alias smuggled through env config flags too.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.env.insert("GIT_CONFIG_COUNT".into(), "1".into());
    host.env
        .insert("GIT_CONFIG_KEY_0".into(), "alias.st".into());
    host.env.insert("GIT_CONFIG_VALUE_0".into(), "!id".into());
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "an env-carried !-alias was not flagged"
    );
    // …while a plain (non-!) alias is just a git subcommand — inert.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file("/home/ubuntu/.gitconfig", "[alias]\n\tco = checkout\n");
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Ok,
        "a plain alias was flagged"
    );
}

#[test]
fn audit_reaches_beyond_the_store_to_all_agent_ground() {
    // The store is not the only armed ground: the agent's home is
    // agent-writable by construction, and a safe.directory rooted
    // there arms whatever checkout the agent builds in it.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[safe]\n\tdirectory = /home/cadence-agent/*\n",
    );
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "safe.directory over the agent home was not flagged"
    );

    // An include into the agent home is agent-written config in the
    // operator's git — same channel as a store include.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[include]\n\tpath = ~cadence-agent/inc\n",
    );
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "an include into the agent's home was not flagged"
    );

    // A world-writable dir under / is agent-reachable too — a missing
    // name beneath it is a config file the agent can plant.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.seed_dir("/shared", 0, 0, 0o777);
    host.write_file(
        "/home/ubuntu/.gitconfig",
        "[include]\n\tpath = /shared/inc\n",
    );
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "an include under world-writable ground was not flagged"
    );
}

#[test]
fn audit_follows_every_include_symlink_hop() {
    // The link chain is resolved hop by hop — the violation is two
    // links deep, and each landing is judged on its own ground.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file("/home/ubuntu/.gitconfig", "[include]\n\tpath = ~/a.inc\n");
    std::os::unix::fs::symlink(
        "/home/ubuntu/b.inc",
        lane.dir.path().join("home/ubuntu/a.inc"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "/var/lib/cadence/evil.inc",
        lane.dir.path().join("home/ubuntu/b.inc"),
    )
    .unwrap();
    host.write_file("/var/lib/cadence/evil.inc", "[safe]\n\tdirectory = *\n");
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "a two-hop include chain into the store was not flagged"
    );

    // A link cycle is unverifiable — fails closed.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.write_file("/home/ubuntu/.gitconfig", "[include]\n\tpath = ~/c1.inc\n");
    std::os::unix::fs::symlink(
        "/home/ubuntu/c2.inc",
        lane.dir.path().join("home/ubuntu/c1.inc"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "/home/ubuntu/c1.inc",
        lane.dir.path().join("home/ubuntu/c2.inc"),
    )
    .unwrap();
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "a cyclic include chain must fail closed"
    );

    // An env-carried include is walked the same way — git honours
    // include.path from GIT_CONFIG_PARAMETERS.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.env.insert(
        "GIT_CONFIG_PARAMETERS".into(),
        "'include.path'='/var/lib/cadence/x.inc'".into(),
    );
    assert_eq!(
        row(&audit(&host), "git-config").level,
        Level::Fail,
        "an env-carried include into the store was not flagged"
    );
}

#[test]
fn audit_and_preflight_refuse_a_foreign_supplementary_group() {
    // `usermod -aG docker cadence-agent` hands the agent a
    // root-equivalent socket — the §5 group set is the only allowance.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    host.groups.insert(
        "docker".to_string(),
        cadence_agent::agent_uid::Group {
            name: "docker".into(),
            gid: 4242,
            members: std::collections::BTreeSet::from([AGENT_USER.to_string()]),
        },
    );
    assert_eq!(
        row(&audit(&host), "agent-groups").level,
        Level::Fail,
        "a foreign supplementary group passed the audit"
    );

    // Preflight refuses it too — provision must not bless the grant.
    let lane = Lane::new();
    let mut host = lane.host();
    run_provision(&mut host, &lane, false); // lands the agent account
    host.groups.insert(
        "sudo".to_string(),
        cadence_agent::agent_uid::Group {
            name: "sudo".into(),
            gid: 4243,
            members: std::collections::BTreeSet::from([AGENT_USER.to_string()]),
        },
    );
    let report = run_provision(&mut host, &lane, false);
    assert!(
        report.refused(),
        "provision re-ran clean over a foreign-group grant"
    );
}

#[test]
fn audit_fails_when_another_account_shares_the_agent_uid() {
    // The passwd-map sweep: a second name answering to the agent's uid
    // is the same collision under another name.
    let lane = Lane::new();
    let mut host = provisioned(&lane);
    let auid = agent_uid(&host);
    host.add_user_record("agent-twin", auid, 4242, "/home/agent-twin", "/bin/sh");
    assert_eq!(
        row(&audit(&host), "agent-user").level,
        Level::Fail,
        "a second account on the agent uid passed the audit"
    );

    // Preflight must refuse the same shape before arming anything.
    let lane = Lane::new();
    let mut host = lane.host();
    run_provision(&mut host, &lane, false);
    let auid = agent_uid(&host);
    host.add_user_record("agent-twin", auid, 4242, "/home/agent-twin", "/bin/sh");
    assert!(
        run_provision(&mut host, &lane, false).refused(),
        "provision blessed a uid collision"
    );
}

#[test]
fn helper_source_refuses_an_acl_write_grant() {
    // A POSIX ACL granting the agent write on the helper source is a
    // write vector modes can't express — the gate must consult it.
    let lane = Lane::new();
    let mut host = lane.host();
    let key = lane.helper_src.display().to_string();
    host.seed_acl(
        &key,
        AclEntry {
            default: false,
            tag: Principal::User(4242),
            perms: 0b111,
        },
    );
    assert!(
        host.helper_source(&lane.helper_src).is_err(),
        "an ACL write grant on the helper source passed the gate"
    );
    assert!(
        run_provision(&mut host, &lane, false).refused(),
        "an ACL-granted helper was installed"
    );

    // The mask gates the grant — a read-only-effective ACL is inert.
    let lane = Lane::new();
    let mut host = lane.host();
    let key = lane.helper_src.display().to_string();
    host.seed_acl(
        &key,
        AclEntry {
            default: false,
            tag: Principal::User(4242),
            perms: 0b111,
        },
    );
    host.seed_acl(
        &key,
        AclEntry {
            default: false,
            tag: Principal::Mask,
            perms: 0b101,
        },
    );
    assert!(
        host.helper_source(&lane.helper_src).is_ok(),
        "a mask-suppressed ACL grant was refused"
    );
}

// ---------- audit: exit surface ----------

#[test]
fn audit_levels_order_and_report_shape() {
    let (_lane, host) = provisioned_fresh();
    let report = audit(&host);
    let names: Vec<&str> = report.rows.iter().map(|r| r.name).collect();
    assert_eq!(
        names,
        [
            "agent-user",
            "agent-groups",
            "agent-home",
            "opt-tree",
            "helper",
            "share-tree",
            "home-acl",
            "git-config"
        ]
    );
    // Every failing row carries a remedy — honest output is part of
    // the contract.
    let (_lane, mut host) = provisioned_fresh();
    host.meta.get_mut(REPOS_DIR).unwrap().mode = 0o777;
    let report = audit(&host);
    for r in &report.rows {
        if r.level != Level::Ok {
            assert!(!r.remedy.is_empty(), "{} failed with no remedy", r.name);
        }
    }
}

// ---------- the plan's own renderable shape ----------

#[test]
fn plan_renders_the_5_script() {
    let lane = Lane::new();
    let mut host = lane.host();
    let (_report, text) = run_provision_text(&mut host, &lane, true);
    for expected in [
        "groupadd --system cadence-agent",
        "groupadd cadence — group absent",
        "groupadd cadence-launch",
        "useradd --system -m -d /home/cadence-agent -s /usr/sbin/nologin -g cadence-agent cadence-agent",
        "usermod -aG cadence ubuntu",
        "usermod -aG cadence cadence-agent",
        "usermod -aG cadence-launch ubuntu",
        "install -d -o cadence-agent -g cadence-agent -m 0750 /home/cadence-agent",
        "install -d -o root -g root -m 0755 /opt/cadence",
        "install -d -o root -g cadence-launch -m 0750 /opt/cadence/libexec",
        "-m 4750",
        "/opt/cadence/libexec/cadence-agent-exec",
        "install -d -o ubuntu -g cadence -m 0750 /var/lib/cadence",
        "install -d -o ubuntu -g cadence -m 0750 /var/lib/cadence/lanes",
        "install -d -o cadence-agent -g cadence -m 2750 /var/lib/cadence/repos",
        "setfacl -m d:g:cadence:rwx /var/lib/cadence/repos",
        "install -d -o ubuntu -g cadence -m 0755 /opt/cadence/bin",
        "install -d -o ubuntu -g cadence -m 0755 /opt/cadence/releases",
    ] {
        assert!(text.contains(expected), "plan missing {expected:?}\n{text}");
    }
}
