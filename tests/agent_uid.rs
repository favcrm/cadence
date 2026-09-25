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
    enforce_root, AclEntry, Meta, Principal, View, AGENT_HOME, AGENT_USER, HELPER_DEST, LANES_DIR,
    LAUNCH_GROUP, LIBEXEC_DIR, NOLOGIN, OPERATOR_USER, OPT_BIN, OPT_RELEASES, OPT_ROOT, REPOS_DIR,
    SHARED_GROUP, VAR_LIB,
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
    let report = run_provision(&mut host, lane, false);
    assert!(!report.refused(), "provision refused on a clean fixture");
    assert!(report.steps.iter().all(|s| s.applied || s.assess.ok()));
    host
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
            provision::cli(false, None, OPERATOR_USER).unwrap(),
            2,
            "non-root caller was not refused"
        );
        assert_eq!(
            provision::cli(true, None, OPERATOR_USER).unwrap(),
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
    // Drifted modes/owners are repaired to §3.
    let mut host = lane.host();
    host.seed_dir(VAR_LIB, 0, 0, 0o777);
    host.seed_dir(REPOS_DIR, 1234, 1234, 0o700);
    run_provision(&mut host, &lane, false);
    assert_eq!(meta(&host, VAR_LIB).mode, 0o750);
    assert_eq!(meta(&host, VAR_LIB).uid, 1000);
    let repos = meta(&host, REPOS_DIR);
    assert_eq!((repos.uid, repos.mode), (agent_uid(&host), 0o2750));

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
    // The install step fails loudly — the rest of §5 still lands.
    assert!(host.stat(HELPER_DEST).unwrap().is_none());
    assert!(!report.clean());
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
    let lane = Lane::new();

    // Agent account: uid 0.
    let mut host = provisioned(&lane);
    let uid = agent_uid(&host);
    host.add_user_record(AGENT_USER, 0, gid(&host, AGENT_USER), AGENT_HOME, NOLOGIN);
    let report = audit(&host);
    assert_eq!(
        row(&report, "agent-user").level,
        Level::Fail,
        "uid-0 agent passed"
    );
    let _ = uid;

    // Agent account: a shell that logs in.
    let mut host = provisioned(&lane);
    let u = host.user(AGENT_USER).unwrap().unwrap();
    host.add_user_record(AGENT_USER, u.uid, u.gid, AGENT_HOME, "/bin/bash");
    assert_eq!(row(&audit(&host), "agent-user").level, Level::Fail);

    // A stray member in the launch edge — `nobody` could now exec as
    // the agent.
    let mut host = provisioned(&lane);
    host.groups
        .get_mut(LAUNCH_GROUP)
        .unwrap()
        .members
        .insert("nobody".to_string());
    assert_eq!(row(&audit(&host), "agent-groups").level, Level::Fail);

    // The agent inside the operator's own group — a traversal grant
    // into ~ wearing a different name.
    let mut host = provisioned(&lane);
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
        (OPT_BIN, 0o750, "opt-tree"),
    ] {
        let mut host = provisioned(&lane);
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
    let mut host = provisioned(&lane);
    host.meta.get_mut(HELPER_DEST).unwrap().mode = 0o750;
    assert_eq!(row(&audit(&host), "helper").level, Level::Fail);
    let mut host = provisioned(&lane);
    host.meta.get_mut(HELPER_DEST).unwrap().uid = 1000;
    assert_eq!(row(&audit(&host), "helper").level, Level::Fail);
    let mut host = provisioned(&lane);
    host.meta.remove(HELPER_DEST);
    std::fs::remove_file(host.root.join(HELPER_DEST.trim_start_matches('/'))).unwrap();
    assert_eq!(row(&audit(&host), "helper").level, Level::Fail);

    // The repos default ACL removed — new checkouts would be
    // operator-unreachable.
    let mut host = provisioned(&lane);
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
    ] {
        let lane = Lane::new();
        let mut host = provisioned(&lane);
        host.write_file("/home/ubuntu/.gitconfig", cfg);
        let report = audit(&host);
        assert_eq!(
            row(&report, "git-config").level,
            Level::Fail,
            "{label}: {cfg:?} was not flagged"
        );
    }
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

// ---------- audit: exit surface ----------

#[test]
fn audit_levels_order_and_report_shape() {
    let lane = Lane::new();
    let host = provisioned(&lane);
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
    let mut host = provisioned(&lane);
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
