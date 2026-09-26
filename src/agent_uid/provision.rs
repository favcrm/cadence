//! `cadence agent-uid provision` — ADR 0007 §5 as an idempotent verb.
//!
//! The verb's whole semantics: **make the host match §5, and nothing
//! else**. Every action checks before it acts, so a second run is a
//! no-op; every action renders as the exact shell line an operator
//! would type, so `--dry-run` is a faithful preview. The action set is
//! deliberately inexpressive — there is no verb that writes under a
//! home, edits git config or touches sudoers — which is how "never
//! writes under `~`, never sets `safe.directory`, never touches
//! sudoers" is a property of the type, not a promise in a comment.
//!
//! Order is the ADR's: accounts and groups first (the filesystem owns
//! names, not numbers), then the §4 negative assertions as a
//! pre-flight — provision refuses to arm a host whose operator git
//! config or home ACLs already reach into the agent domain — then the
//! directory trees and the setuid helper.

use std::io::Write;
use std::path::PathBuf;

use crate::error::{Error, Result};

use super::{
    audit, caller_uids, enforce_root, AgentPrincipals, Host, LiveHost, Meta, NewUser, Principal,
    View, AGENT_HOME, AGENT_USER, HELPER_DEST, HOME_ACL_WALK_BUDGET, LANES_DIR, LAUNCH_GROUP,
    LIBEXEC_DIR, NOLOGIN, OPT_BIN, OPT_RELEASES, OPT_ROOT, REPOS_DEFAULT_ACL_PERMS, REPOS_DIR,
    SHARED_GROUP, VAR_LIB,
};

/// The operator-run command. `helper` is the built
/// `cadence-agent-exec` to install — an explicit absolute path, never
/// discovered: under sudo the cwd is agent-writable, so a
/// cwd-relative fallback would install whatever an agent dropped
/// there (C2). `None` is for spec-only consumers (the audit) that
/// assess the installed file without a source at hand.
pub struct Spec {
    pub operator: String,
    pub helper: Option<PathBuf>,
    pub dry_run: bool,
}

/// One §5 line. `render` is the shell equivalent — what `--dry-run`
/// prints and what the runbook documents.
#[derive(Clone, Debug)]
pub enum Action {
    /// `groupadd [--system] <name>` — ensure the group exists.
    Group { name: &'static str, system: bool },
    /// `useradd --system -m -d <home> -s <shell> -g <group> <name>` —
    /// the account itself; drift on an existing account is repaired
    /// with the equivalent `usermod`.
    User,
    /// `usermod -aG <group> <user>` — supplementary membership.
    Member { group: &'static str, user: String },
    /// `install -d -o <owner> -g <group> -m <mode> <path>` — one
    /// directory each, so no mode is ever shared across targets.
    Dir {
        path: &'static str,
        owner: String,
        group: &'static str,
        mode: u32,
    },
    /// `install -o <owner> -g <group> -m <mode> <src> <dest>` — the
    /// helper binary, mode 4750.
    Install {
        src: Option<PathBuf>,
        dest: &'static str,
        owner: &'static str,
        group: &'static str,
        mode: u32,
    },
    /// `setfacl -m d:g:<group>:<perms> <path>` — the repos default ACL.
    DefaultAcl {
        path: &'static str,
        group: &'static str,
        perms: u8,
    },
}

/// What `assess` found on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Assess {
    /// Already exactly as specified.
    Clean,
    /// Absent — apply creates it.
    Needed(String),
    /// Present but wrong — apply repairs it (`install` semantics).
    Drift(String),
    /// Present and wrong in a way provision refuses to touch — the
    /// operator looks at it by hand.
    Blocked(String),
}

impl Assess {
    pub fn ok(&self) -> bool {
        matches!(self, Assess::Clean)
    }
}

fn uid_of(view: &dyn View, name: &str) -> std::io::Result<Option<u32>> {
    Ok(view.user(name)?.map(|u| u.uid))
}

fn gid_of(view: &dyn View, name: &str) -> std::io::Result<Option<u32>> {
    Ok(view.group(name)?.map(|g| g.gid))
}

impl Action {
    /// The exact shell line — shown by `--dry-run` and quoted by the
    /// runbook. `<helper>` stands in when no binary was resolved.
    pub fn render(&self) -> String {
        match self {
            Action::Group { name, system } => {
                if *system {
                    format!("groupadd --system {name}")
                } else {
                    format!("groupadd {name}")
                }
            }
            Action::User => format!(
                "useradd --system -m -d {AGENT_HOME} -s {NOLOGIN} -g {AGENT_USER} {AGENT_USER}"
            ),
            Action::Member { group, user } => format!("usermod -aG {group} {user}"),
            Action::Dir {
                path,
                owner,
                group,
                mode,
            } => format!("install -d -o {owner} -g {group} -m {mode:04o} {path}"),
            Action::Install {
                src,
                dest,
                owner,
                group,
                mode,
            } => {
                let src = src
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<helper>".to_string());
                format!("install -o {owner} -g {group} -m {mode:04o} {src} {dest}")
            }
            Action::DefaultAcl { path, group, perms } => {
                const RWX: [&str; 8] = ["---", "--x", "-w-", "-wx", "r--", "r-x", "rw-", "rwx"];
                format!(
                    "setfacl -m d:g:{group}:{} {path}",
                    RWX[(perms & 7) as usize]
                )
            }
        }
    }

    /// Paths this action writes — the never-under-`~` test iterates
    /// exactly this set.
    pub fn targets(&self) -> Vec<String> {
        match self {
            Action::Dir { path, .. } | Action::DefaultAcl { path, .. } => {
                vec![(*path).to_string()]
            }
            Action::Install { dest, .. } => vec![(*dest).to_string()],
            Action::Group { .. } | Action::User | Action::Member { .. } => Vec::new(),
        }
    }

    /// Compare the spec against the live host. `pub(crate)` — the
    /// audit runs the same assessment so a check cannot drift from
    /// what provision installs.
    pub(crate) fn assess(&self, view: &dyn View) -> Result<Assess> {
        match self {
            Action::Group { name, .. } => Ok(match view.group(name)? {
                Some(_) => Assess::Clean,
                None => Assess::Needed("group absent".into()),
            }),
            Action::User => match view.user(AGENT_USER)? {
                None => Ok(Assess::Needed("account absent".into())),
                Some(u) => {
                    if u.uid == 0 {
                        return Ok(Assess::Blocked(format!(
                            "{AGENT_USER} resolves to uid 0 — refusing to bless it as the \
                             agent account"
                        )));
                    }
                    let mut drift = Vec::new();
                    if u.home != AGENT_HOME {
                        drift.push(format!("home {} != {AGENT_HOME}", u.home));
                    }
                    if u.shell != NOLOGIN {
                        drift.push(format!("shell {} != {NOLOGIN}", u.shell));
                    }
                    let primary = gid_of(view, AGENT_USER)?;
                    if primary.is_some_and(|g| u.gid != g) {
                        drift.push(format!("primary gid {} != {AGENT_USER}", u.gid));
                    }
                    if drift.is_empty() {
                        Ok(Assess::Clean)
                    } else {
                        Ok(Assess::Drift(drift.join(", ")))
                    }
                }
            },
            Action::Member { group, user } => {
                // Absent mid-plan is not a refusal — the Group action
                // above creates it (dry-run assesses without applying).
                // If it is genuinely absent at apply time, `usermod`
                // fails loudly there.
                let Some(g) = view.group(group)? else {
                    return Ok(Assess::Needed(format!(
                        "group {group} pending — created above"
                    )));
                };
                // Membership is the whole group vector —
                // `getgrouplist` folds supplementary and primary-gid
                // membership into one check, so neither spelling slips
                // a redundant `usermod -aG` nor misses a satisfied
                // edge.
                if let Some(u) = view.user(user)? {
                    if view.member_gids(&u)?.contains(&g.gid) {
                        return Ok(Assess::Clean);
                    }
                }
                Ok(Assess::Needed(format!("{user} is not in {group}")))
            }
            Action::Dir {
                path,
                owner,
                group,
                mode,
            } => {
                let Some(meta) = view.stat(path)? else {
                    return Ok(Assess::Needed("absent".into()));
                };
                if meta.is_symlink {
                    return Ok(Assess::Blocked(format!(
                        "{path} is a symlink — refusing to touch it"
                    )));
                }
                if !meta.is_dir {
                    return Ok(Assess::Blocked(format!(
                        "{path} exists and is not a directory"
                    )));
                }
                assess_meta(view, path, meta, owner, group, *mode)
            }
            Action::Install {
                src,
                dest,
                owner,
                group,
                mode,
            } => {
                let Some(meta) = view.stat(dest)? else {
                    return Ok(Assess::Needed("absent".into()));
                };
                if meta.is_symlink {
                    return Ok(Assess::Blocked(format!(
                        "{dest} is a symlink — refusing to install over it"
                    )));
                }
                if !meta.is_file {
                    return Ok(Assess::Blocked(format!("{dest} exists and is not a file")));
                }
                match assess_meta(view, dest, meta, owner, group, *mode)? {
                    Assess::Clean => {}
                    other => return Ok(other),
                }
                // Owner/group/mode match; with a resolved source the
                // bytes decide clean vs refresh — a rebuilt helper is
                // reinstalled. The source is vetted at assess too, so
                // a bad --helper refuses here, not just at apply.
                match src {
                    None => Ok(Assess::Clean),
                    Some(src) => {
                        // Vet + hash on the same opened fd — no
                        // check→read window for a source swap.
                        let want = match view.helper_source(src) {
                            Ok(hash) => hash,
                            Err(e) => return Ok(Assess::Blocked(format!("{e}"))),
                        };
                        match view.file_sha256(dest) {
                            Ok(have) if have == want => Ok(Assess::Clean),
                            _ => Ok(Assess::Drift("content differs".into())),
                        }
                    }
                }
            }
            Action::DefaultAcl { path, group, perms } => {
                let Some(gid) = gid_of(view, group)? else {
                    return Ok(Assess::Needed(format!(
                        "group {group} pending — created above"
                    )));
                };
                let present = view
                    .acls(path)?
                    .iter()
                    .any(|e| e.default && e.tag == Principal::Group(gid) && e.perms == *perms);
                Ok(if present {
                    Assess::Clean
                } else {
                    Assess::Needed(format!("no default {group} entry on {path}"))
                })
            }
        }
    }

    fn apply(&self, host: &mut dyn Host) -> Result<()> {
        match self {
            Action::Group { name, system } => host.create_group(name, *system)?,
            Action::User => {
                let spec = NewUser {
                    name: AGENT_USER.into(),
                    home: AGENT_HOME.into(),
                    shell: NOLOGIN.into(),
                    primary_group: AGENT_USER.into(),
                };
                // Assessed Drift → repair; the double-lookup is the
                // honest read of `useradd` vs `usermod`.
                if host.user(AGENT_USER)?.is_some() {
                    host.repair_user(&spec)?;
                } else {
                    host.create_user(&spec)?;
                }
            }
            Action::Member { group, user } => host.add_member(group, user)?,
            Action::Dir {
                path,
                owner,
                group,
                mode,
            } => {
                let prior = host.stat(path)?;
                if prior.is_none() {
                    host.mkdir(path)?;
                }
                // Permission bits apply as the intersection — bits the
                // operator removed stay removed. Special bits union:
                // spec-required ones get restored (setgid on repos is
                // functional, not a grant) and operator-added ones are
                // kept.
                let effective = prior
                    .map(|m| ((mode | m.mode) & 0o7000) | (mode & m.mode & 0o777))
                    .unwrap_or(*mode);
                host.set_meta(path, owner, group, effective)?;
            }
            Action::Install {
                src,
                dest,
                owner,
                group,
                mode,
            } => {
                let src = src.as_ref().ok_or_else(|| {
                    Error::rejected(
                        "no cadence-agent-exec binary — build it and pass --helper <path>"
                            .to_string(),
                    )
                })?;
                host.install(src, dest, owner, group, *mode)?;
            }
            Action::DefaultAcl { path, group, perms } => {
                host.set_default_group_acl(path, group, *perms)?
            }
        }
        Ok(())
    }
}

/// owner/group/mode comparison, resolving names through the view.
///
/// Re-provision only ever *tightens* (§7): a current owner that
/// resolves to a different account name is a refusal — chowning a
/// foreign account's tree is not provision's call — while an
/// unresolvable uid is drift it may repair. A mode strictly tighter
/// than spec is the operator's own hardening and is left alone; a
/// looser mode is repaired; one that is neither tighter nor looser
/// (grants one bit while dropping another) is a refusal — that shape
/// means someone deviated on purpose and provision must not guess.
fn assess_meta(
    view: &dyn View,
    path: &str,
    meta: Meta,
    owner: &str,
    group: &str,
    mode: u32,
) -> Result<Assess> {
    let mut drift = Vec::new();
    let want_uid = uid_of(view, owner)?
        .ok_or_else(|| Error::internal(format!("owner {owner} does not resolve")))?;
    let want_gid = gid_of(view, group)?
        .ok_or_else(|| Error::internal(format!("group {group} does not resolve")))?;
    if meta.uid != want_uid {
        match view.user_name(meta.uid)? {
            Some(name) if name == owner => {
                drift.push(format!("owner uid {} — {owner}'s uid moved", meta.uid))
            }
            // A root-owned tree is the caller's own — reclaiming it is
            // provision's job. Any other resolved account owns its
            // files: refuse rather than chown a foreign tree.
            Some(_) if meta.uid == 0 => drift.push(format!("owner root != {owner}")),
            Some(name) => {
                return Ok(Assess::Blocked(format!(
                    "{path} is owned by {name} — refusing to chown a foreign account's tree"
                )))
            }
            None => drift.push(format!("owner uid {} != {owner}", meta.uid)),
        }
    }
    if meta.gid != want_gid {
        match view.group_name(meta.gid)? {
            Some(name) if name == group => {
                drift.push(format!("group gid {} — {group}'s gid moved", meta.gid))
            }
            Some(_) if meta.gid == 0 => drift.push(format!("group root != {group}")),
            Some(name) => {
                return Ok(Assess::Blocked(format!(
                    "{path} is group-owned by {name} — refusing to chgrp a foreign group's tree"
                )))
            }
            None => drift.push(format!("group gid {} != {group}", meta.gid)),
        }
    }
    // Permission bits (0777) are tighten-only: extra grants repair,
    // missing bits are the operator's own hardening and stand.
    // Special bits (setuid/setgid/sticky, 07000) are functional, not
    // grants — a spec bit that was dropped is drift to repair; an
    // extra one is the operator's addition and also stands.
    let extra = (meta.mode & 0o777) & !(mode & 0o777);
    let missing = (mode & 0o777) & !(meta.mode & 0o777);
    let dropped_special = (mode & 0o7000) & !(meta.mode & 0o7000);
    match (extra, missing) {
        (0, _) => {} // tighter or equal perms — the operator's hardening stands
        (_, 0) => drift.push(format!("mode {:04o} is looser than {mode:04o}", meta.mode)),
        (_, _) => {
            return Ok(Assess::Blocked(format!(
                "{path} mode {:04o} neither contains nor fits spec {mode:04o} — refusing",
                meta.mode
            )))
        }
    }
    if dropped_special != 0 {
        drift.push(format!(
            "mode {:04o} dropped spec special bits {:04o}",
            meta.mode, dropped_special
        ));
    }
    if drift.is_empty() {
        Ok(Assess::Clean)
    } else {
        Ok(Assess::Drift(format!("{path}: {}", drift.join(", "))))
    }
}

/// The full §5 sequence, ordered. The cut between `accounts` and `fs`
/// is where the §4 pre-flight lands: the agent uid must resolve
/// before the negative assertions can test numeric principals.
pub(crate) fn plan(spec: &Spec) -> (Vec<Action>, Vec<Action>) {
    let accounts = vec![
        Action::Group {
            name: AGENT_USER,
            system: true,
        },
        Action::Group {
            name: SHARED_GROUP,
            system: false,
        },
        Action::Group {
            name: LAUNCH_GROUP,
            system: false,
        },
        Action::User,
        Action::Member {
            group: SHARED_GROUP,
            user: spec.operator.clone(),
        },
        Action::Member {
            group: SHARED_GROUP,
            user: AGENT_USER.to_string(),
        },
        Action::Member {
            group: LAUNCH_GROUP,
            user: spec.operator.clone(),
        },
    ];
    let fs = vec![
        Action::Dir {
            path: AGENT_HOME,
            owner: AGENT_USER.to_string(),
            group: AGENT_USER,
            mode: 0o750,
        },
        Action::Dir {
            path: OPT_ROOT,
            owner: "root".into(),
            group: "root",
            mode: 0o755,
        },
        Action::Dir {
            path: LIBEXEC_DIR,
            owner: "root".into(),
            group: LAUNCH_GROUP,
            mode: 0o750,
        },
        Action::Install {
            src: spec.helper.clone(),
            dest: HELPER_DEST,
            owner: "root",
            group: LAUNCH_GROUP,
            mode: 0o4750,
        },
        Action::Dir {
            path: VAR_LIB,
            owner: spec.operator.clone(),
            group: SHARED_GROUP,
            mode: 0o750,
        },
        Action::Dir {
            path: LANES_DIR,
            owner: spec.operator.clone(),
            group: SHARED_GROUP,
            mode: 0o750,
        },
        Action::Dir {
            path: REPOS_DIR,
            owner: AGENT_USER.into(),
            group: SHARED_GROUP,
            mode: 0o2750,
        },
        Action::DefaultAcl {
            path: REPOS_DIR,
            group: SHARED_GROUP,
            perms: REPOS_DEFAULT_ACL_PERMS,
        },
        Action::Dir {
            path: OPT_BIN,
            owner: spec.operator.clone(),
            group: SHARED_GROUP,
            mode: 0o755,
        },
        Action::Dir {
            path: OPT_RELEASES,
            owner: spec.operator.clone(),
            group: SHARED_GROUP,
            mode: 0o755,
        },
    ];
    (accounts, fs)
}

/// One plan line's outcome, for the report and the summary.
pub struct StepResult {
    pub action: Action,
    pub assess: Assess,
    pub applied: bool,
}

pub struct Report {
    pub steps: Vec<StepResult>,
    /// §4 pre-flight findings — non-empty refuses the fs phase.
    pub preflight: Vec<String>,
    /// An apply failed anywhere — including the last action, which
    /// must not leave the exit status at 0.
    pub failed: bool,
    pub dry_run: bool,
}

impl Report {
    /// Every artifact already as specified (or would be, in dry-run).
    pub fn clean(&self) -> bool {
        self.steps.iter().all(|s| s.assess.ok()) && self.preflight.is_empty() && !self.failed
    }

    pub fn refused(&self) -> bool {
        self.failed
            || !self.preflight.is_empty()
            || self
                .steps
                .iter()
                .any(|s| matches!(s.assess, Assess::Blocked(_)))
    }
}

/// §4's pre-flight: the negative assertions, run after the account
/// phase so the agent uid/gids resolve. A violation refuses the fs
/// phase — provision never arms a host whose operator side already
/// reaches into the agent domain.
fn preflight(view: &dyn View, operator: &str) -> Vec<String> {
    let principals = AgentPrincipals::resolve(view).unwrap_or(AgentPrincipals {
        uid: None,
        gids: Vec::new(),
        group_names: [AGENT_USER, SHARED_GROUP],
    });
    let mut findings = Vec::new();
    // The agent's uid must be its own — an agent sharing the
    // operator's uid is no boundary at all.
    if let (Ok(Some(agent)), Ok(Some(op))) = (view.user(AGENT_USER), view.user(operator)) {
        if agent.uid == op.uid {
            findings.push(format!(
                "{AGENT_USER} resolves to uid {} — the operator's own uid — refusing",
                agent.uid
            ));
        }
        if agent.uid == 0 {
            findings.push(format!("{AGENT_USER} resolves to uid 0 — refusing"));
        }
        // A second account answering to the agent's uid is the same
        // collision under another name — the audit sweeps the passwd
        // map for it; provision must not bless it either.
        if let Ok(users) = view.users() {
            for other in users {
                if other.uid == agent.uid && other.name != AGENT_USER && other.name != operator {
                    findings.push(format!(
                        "uid {} is shared with account {} — refusing",
                        agent.uid, other.name
                    ));
                }
            }
        }
        // Foreign supplementary groups are grants the spec never
        // made: `usermod -aG docker cadence-agent` is a
        // root-equivalent reach. The §5 set is the primary gid and
        // the shared group only.
        let shared = view.group(SHARED_GROUP).ok().flatten().map(|g| g.gid);
        if let Ok(gids) = view.member_gids(&agent) {
            for gid in gids {
                if gid == agent.gid || Some(gid) == shared {
                    continue;
                }
                let name = view
                    .group_name(gid)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| format!("gid {gid}"));
                findings.push(format!(
                    "{AGENT_USER} is a member of {name} — outside the §5 group set — refusing"
                ));
            }
        }
        // The launch edge must already hold the operator — the setuid
        // helper gates on `cadence-launch` membership, so a missing
        // operator row means the tree we are about to arm cannot be
        // used by the seat it was built for.
        if let Ok(Some(g)) = view.group(LAUNCH_GROUP) {
            let in_launch = view
                .member_gids(&op)
                .map(|gids| gids.contains(&g.gid))
                .unwrap_or(false);
            if !in_launch {
                findings.push(format!(
                    "operator {operator} is not in {LAUNCH_GROUP} — the helper's gate would \
                     refuse the very seat provision is arming"
                ));
            }
        }
    }
    findings.extend(audit::home_acl_findings(
        view,
        operator,
        &principals,
        HOME_ACL_WALK_BUDGET,
    ));
    let cfg = audit::git_config_audit(view, operator, &principals);
    findings.extend(cfg.findings);
    // Can't-prove is a refusal too — provision never arms a host it
    // could not fully audit.
    findings.extend(
        cfg.unverified
            .into_iter()
            .map(|u| format!("unverifiable: {u}")),
    );
    findings
}

fn run_phase(
    actions: &[Action],
    spec: &Spec,
    host: &mut dyn Host,
    out: &mut dyn Write,
    steps: &mut Vec<StepResult>,
    failed: &mut bool,
) -> Result<()> {
    for action in actions {
        if *failed {
            steps.push(StepResult {
                action: action.clone(),
                assess: Assess::Blocked("skipped — earlier step failed".into()),
                applied: false,
            });
            let _ = writeln!(out, "  skip   {}", action.render());
            continue;
        }
        let assess = match action.assess(host.as_view()) {
            Ok(a) => a,
            // An assess that cannot even read the host refuses the
            // run — loudly, with the reason — rather than aborting.
            Err(e) => Assess::Blocked(format!("cannot assess: {e}")),
        };
        match &assess {
            Assess::Clean => {
                let _ = writeln!(out, "  ok     {}", action.render());
            }
            Assess::Blocked(why) => {
                let _ = writeln!(out, "  REFUSE {} — {why}", action.render());
                *failed = true;
            }
            Assess::Needed(why) | Assess::Drift(why) => {
                let verb = if matches!(assess, Assess::Drift(_)) {
                    "fix"
                } else {
                    "create"
                };
                let _ = writeln!(out, "  {verb:6} {} — {why}", action.render());
                if !spec.dry_run {
                    if let Err(e) = action.apply(host) {
                        let _ = writeln!(out, "  FAIL   {} — {e}", action.render());
                        *failed = true;
                    }
                }
            }
        }
        let applied =
            !spec.dry_run && matches!(assess, Assess::Needed(_) | Assess::Drift(_)) && !*failed;
        steps.push(StepResult {
            action: action.clone(),
            assess,
            applied,
        });
    }
    Ok(())
}

/// The engine: assess → (pre-flight) → apply, printing one line per
/// action to `out`. Returns the report; `cli` maps it to an exit code.
pub fn run(host: &mut dyn Host, spec: &Spec, out: &mut dyn Write) -> Result<Report> {
    let (accounts, fs) = plan(spec);
    let mut steps = Vec::new();
    let mut failed = false;

    writeln!(
        out,
        "agent-uid provision{}:",
        if spec.dry_run { " (dry-run)" } else { "" }
    )?;
    run_phase(&accounts, spec, host, out, &mut steps, &mut failed)?;

    let findings = if failed {
        Vec::new()
    } else {
        preflight(host.as_view(), &spec.operator)
    };
    if !findings.is_empty() {
        writeln!(
            out,
            "refused: the operator side already reaches the agent domain:"
        )?;
        for f in &findings {
            writeln!(out, "  - {f}")?;
        }
        writeln!(
            out,
            "fix those first (§4 rule 2 — no uid-1000 git config covering \
             {VAR_LIB}, no agent-reachable ACL under ~), then re-run."
        )?;
    }
    let refuse = failed || !findings.is_empty();
    if !refuse {
        run_phase(&fs, spec, host, out, &mut steps, &mut failed)?;
    }

    if spec.dry_run {
        writeln!(
            out,
            "dry-run: nothing was changed{}",
            if refuse {
                " — the run itself would refuse"
            } else {
                ""
            }
        )?;
    } else if refuse || failed {
        writeln!(out, "provision incomplete — see refusals above")?;
    } else {
        writeln!(
            out,
            "provision complete — audit with `cadence agent-uid doctor`"
        )?;
    }
    Ok(Report {
        steps,
        preflight: findings,
        failed,
        dry_run: spec.dry_run,
    })
}

/// `cadence agent-uid provision` — root gate first, then the engine
/// against the live host. Refusal is exit 2, like the helper's.
pub fn cli(dry_run: bool, helper: PathBuf, operator: &str) -> Result<i32> {
    let (uid, euid) = caller_uids();
    if let Err(e) = enforce_root(uid, euid) {
        eprintln!("cadence agent-uid provision: refused: {e}");
        return Ok(2);
    }
    // The helper source is the setuid bridge the boundary rests on —
    // only an explicit absolute path is trusted. No cwd-relative
    // discovery: under sudo the cwd may be agent-writable.
    if !helper.is_absolute() {
        eprintln!(
            "cadence agent-uid provision: refused: --helper {} must be an absolute path",
            helper.display()
        );
        return Ok(2);
    }
    let mut host = LiveHost::new();
    // The operator must resolve — it names groups and owns trees, and
    // its uid joins the trusted set (`/var/lib/cadence` is operator-
    // owned by spec, so the pinned write path must accept it). An
    // unresolvable name would only fail deep inside the run — refuse
    // it up front, where the error is legible.
    let op = match host.user(operator) {
        Ok(Some(op)) => op,
        _ => {
            eprintln!(
                "cadence agent-uid provision: refused: operator account {operator} does not resolve"
            );
            return Ok(2);
        }
    };
    host.trust_uid(op.uid);
    let spec = Spec {
        operator: operator.to_string(),
        helper: Some(helper),
        dry_run,
    };
    let report = run(&mut host, &spec, &mut std::io::stdout())?;
    Ok(if report.refused() { 2 } else { 0 })
}

/// Every path a plan touches must live in one of these roots — the
/// "never under `~`, never sudoers" invariant as a testable fact.
pub const ALLOWED_WRITE_ROOTS: &[&str] = &[AGENT_HOME, OPT_ROOT, VAR_LIB];

/// The plan's declared write set — the boundary test asserts every
/// entry sits under [`ALLOWED_WRITE_ROOTS`].
pub fn plan_writes(spec: &Spec) -> Vec<String> {
    let (a, f) = plan(spec);
    a.iter().chain(f.iter()).flat_map(Action::targets).collect()
}
