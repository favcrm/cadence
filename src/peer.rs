//! Connection-bound caller identity — the one rule the daemon's Unix
//! socket and the board's TCP writes share (CAD-102, CAD-254, CAD-263).
//!
//! A peer *process* is tied to a registered pane by three signals
//! ([`PeerTies`]), and only ONE of them is unforgeable:
//!
//! - the pane's pid on the peer's `/proc` ancestry — the kernel keeps
//!   it; a process can leave a pane's ancestry (`setsid` + fork) but
//!   never join another's;
//! - the pane's pty among the peer's stdio fds (a `setsid` detach keeps
//!   stdio) — caller-CHOOSABLE by any same-uid process: it can open
//!   another pane's `/dev/pts/N` (the user owns it) onto its stdio;
//! - the pane's `CADENCE_ALIAS` in the peer's environment (a detach
//!   keeps that too) — caller-choosable: any process can export
//!   `CADENCE_ALIAS=B`.
//!
//! - `agent answer` uses all three ([`PeerTies::tied_to`],
//!   [`PeerTies::agents`]): a caller tied to the target pane by any of
//!   them is refused, and the audit `by` names the first other pane it
//!   is tied to. A forged signal can only narrow there — it refuses
//!   its forger or mislabels an audit stamp; it authorizes nothing.
//! - A board write is attributed to an agent on ancestry or the pty tie
//!   ([`PeerTies::attributed_agents`]): an env alias alone attributes
//!   nothing, so `CADENCE_ALIAS=B curl …` from a pane-less process
//!   writes as `operator (ui)`, not as agent B.
//! - A board write is also attributed to a MANAGED endpoint (a claude or
//!   codex provider the daemon launched, no pane — CAD-335) when the
//!   provider's recorded pid is on the peer's ancestry
//!   ([`AgentRoots::managed`]). Ancestry is the only managed signal:
//!   the provider's stdio is pipes and log files, so there is no pty.
//!
//! Accepted residual (CAD-276): the pty tie is kept because it is what
//! attributes a `setsid` child of a pane to its agent instead of to
//! `operator (ui)` — dropping it would be an escalation (operator >
//! agent). The price is LATERAL authorship forgery: a same-uid off-pane
//! process that opens another pane's `/dev/pts/N` onto its stdio is
//! attributed as that pane's agent. It gains no privilege over what it
//! already had as `operator (ui)`; it only chooses whose name a write
//! carries. The root weakness — an unattributable local caller defaults
//! to `operator` — is CAD-335 phase 2 (operator-by-positive-proof, with
//! ADR 0004's operator session); `slot_reconcile` already requires
//! positive proof ([`operator_proof`]).
//!
//! The daemon names its Unix peer through `SO_PEERCRED`; a TCP
//! connection carries no credentials, so the board recovers the peer
//! the way `ss -p` does: the connection's client-side socket in
//! `/proc/net/tcp{,6}` gives its inode, and whichever process holds
//! `socket:[inode]` in `/proc/<pid>/fd` is the peer.
//!
//! Only processes this user can inspect are visible: a socket held by
//! another user's process (a root proxy) cannot be attributed, and the
//! caller decides what that means — the board refuses the write. The
//! one foreign peer the board accepts is the `tailscale serve` proxy,
//! proven by [`crate::tailnet_proof`] from the owner uid
//! [`client_socket`] reads, which `/proc/net/tcp` shows for any user.

use std::collections::{BTreeSet, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::adapter::pty::caller_chain;
use crate::error::Error;

/// What ties one peer process to registered panes — the three signals
/// of the caller rule, read once per peer. `chain` is `None` when any
/// `/proc` read failed mid-walk: an incomplete chain proves neither
/// membership nor its absence, so callers must treat it as
/// unverifiable rather than outside.
pub(crate) struct PeerTies {
    pid: u32,
    chain: Option<Vec<u32>>,
    env_alias: Option<String>,
}

impl PeerTies {
    /// Read the peer's ancestry and `CADENCE_ALIAS` now.
    pub(crate) fn probe(pid: u32) -> Self {
        Self {
            pid,
            chain: caller_chain(pid),
            env_alias: caller_env_alias(pid),
        }
    }

    /// Whether the whole `/proc` ancestry could be walked.
    pub(crate) fn walked(&self) -> bool {
        self.chain.is_some()
    }

    /// `pid` is the peer itself or on its `/proc` ancestry — the one
    /// unforgeable signal: a process can leave an ancestry but never
    /// join another's.
    fn descends_from(&self, pid: u32) -> bool {
        self.chain.as_deref().is_some_and(|c| c.contains(&pid))
    }

    /// The process signals: the pane pid is on the peer's ancestry
    /// (unforgeable), or the peer holds the pane's pty on its stdio
    /// (choosable by any same-uid process — the accepted lateral
    /// residual in the module doc).
    fn process_tied(&self, pane_pid: u32) -> bool {
        self.descends_from(pane_pid) || holds_pane_tty(self.pid, pane_pid)
    }

    /// The widest tie — a process signal OR the pane's `CADENCE_ALIAS`
    /// in the peer's env. For narrowing authority only (a pane may not
    /// answer its own menu): a forged alias can only refuse its forger.
    pub(crate) fn tied_to(&self, alias: &str, pane_pid: u32) -> bool {
        self.process_tied(pane_pid) || self.env_alias.as_deref() == Some(alias)
    }

    /// The registered panes (`(alias, pane_pid)`) whose agent the peer
    /// may be ATTRIBUTED as — process signals only, sorted by alias
    /// and deduplicated. `CADENCE_ALIAS` is caller-chosen, so an env
    /// tie never attributes by itself; when a process signal
    /// corroborates it, the process signal already decides. The pty
    /// signal is choosable too (see the module doc's residual).
    pub(crate) fn attributed_agents<'a>(
        &self,
        panes: impl IntoIterator<Item = (&'a str, u32)>,
    ) -> Vec<String> {
        let tied: BTreeSet<&str> = panes
            .into_iter()
            .filter(|(_, pane_pid)| self.process_tied(*pane_pid))
            .map(|(alias, _)| alias)
            .collect();
        tied.into_iter().map(str::to_string).collect()
    }

    /// Every registered pane (`(alias, pane_pid)`) the peer is tied to,
    /// sorted by alias and deduplicated — deterministic, never map
    /// order.
    pub(crate) fn agents<'a>(
        &self,
        panes: impl IntoIterator<Item = (&'a str, u32)>,
    ) -> Vec<String> {
        let tied: BTreeSet<&str> = panes
            .into_iter()
            .filter(|(alias, pane_pid)| self.tied_to(alias, *pane_pid))
            .map(|(alias, _)| alias)
            .collect();
        tied.into_iter().map(str::to_string).collect()
    }

    /// Does the peer hold a pty at all? Meaningful only after every
    /// pane membership check failed, so any pts fd is foreign by
    /// definition — positive evidence of an interactive terminal. A
    /// detached caller (`setsid … </dev/null >&2`) holds none.
    pub(crate) fn on_tty(&self) -> bool {
        (0..=2).any(|fd| {
            std::fs::read_link(format!("/proc/{}/fd/{fd}", self.pid))
                .is_ok_and(|p| p.to_string_lossy().starts_with("/dev/pts/"))
        })
    }
}

/// The `CADENCE_ALIAS` the peer carries — pane env survives `setsid`,
/// so a detached pane process still names its agent. An alias this
/// daemon never registered means nothing (a stale or foreign daemon's
/// env) — only registered panes match.
fn caller_env_alias(peer_pid: u32) -> Option<String> {
    let env = std::fs::read(format!("/proc/{peer_pid}/environ")).ok()?;
    env.split(|b| *b == 0)
        .filter_map(|kv| std::str::from_utf8(kv).ok())
        .find_map(|kv| kv.strip_prefix("CADENCE_ALIAS="))
        .filter(|a| !a.is_empty())
        .map(str::to_string)
}

/// Does `peer_pid` hold the pane's pty? A detached pane process loses
/// its ancestry and controlling terminal but keeps stdio — the fd
/// targets still name the pane's pts device. Two residuals: redirected
/// stdio escapes the tie (a maximal-effort detach, accepted because
/// the same actor could `tmux send-keys` its own pane directly), and a
/// same-uid process can open ANOTHER pane's pts onto its stdio and
/// satisfy it (lateral attribution, accepted — see the module doc).
fn holds_pane_tty(peer_pid: u32, pane_pid: u32) -> bool {
    let pane_tty = (0..=2)
        .filter_map(|fd| std::fs::read_link(format!("/proc/{pane_pid}/fd/{fd}")).ok())
        .find(|p| p.to_string_lossy().starts_with("/dev/pts/"));
    let Some(tty) = pane_tty else {
        return false;
    };
    (0..=2)
        .any(|fd| std::fs::read_link(format!("/proc/{peer_pid}/fd/{fd}")).is_ok_and(|p| p == tty))
}

/// The caller matched no pane — place it honestly for a pane-attention
/// verb. A broken `/proc` walk (`walked == false`) proves neither
/// membership nor its absence: a refusal while the target pane is
/// alive, `unknown` once it is gone. A clean walk with no match is
/// `operator` only with positive terminal evidence (`foreign_tty`); a
/// detached caller is `unknown`, never `operator`.
pub(crate) fn unmatched_caller(
    walked: bool,
    target_alive: bool,
    foreign_tty: bool,
    verb: &str,
) -> crate::Result<(String, &'static str)> {
    if !walked {
        if target_alive {
            return Err(Error::rejected(format!(
                "cannot derive the caller for `{verb}` — /proc could \
                 not be walked while the target pane is alive; run it \
                 from a shell attached to a pane or outside all panes",
            )));
        }
        return Ok(("unknown".to_string(), "unknown"));
    }
    if foreign_tty {
        Ok(("operator".to_string(), "operator"))
    } else {
        Ok(("unknown".to_string(), "unknown"))
    }
}

/// Positive proof that a local peer is the operator — the gate for
/// `slot_reconcile` (CAD-276). Being tied to no agent is not proof: a
/// `setsid` + double-fork detach of a pane or managed tool is tied to
/// nothing. The peer is the operator only when each check reads
/// cleanly and holds, in this order (`Err` names the first failure):
///
/// 1. it runs as `uid` (real and effective) — the daemon's uid;
/// 2. its whole `/proc` ancestry can be walked, every hop's `status`
///    readable;
/// 3. no hop is a registered pane pid (`panes`), an enrolled or
///    tombstoned managed-endpoint root (`enrolled_root`), or a strict
///    descendant of `daemon_pid` (every process the daemon launched).
///    Under `daemon run` the daemon is the child subreaper of all of
///    them (CAD-308, [`crate::reaper`]): a process that detaches from
///    a tree the daemon launched — `setsid -f`, a double fork — is
///    re-parented to the daemon, not to init, so it stays a descendant
///    and this check refuses it whatever its env, session or stdio;
/// 4. no hop of this uid carries `CADENCE_ALIAS` (an agent's) or
///    `CADENCE_RUNNER_ID` (a daemon-launched runner's) in its environment,
///    and the peer's own environment is readable. An ancestor's
///    unreadable environment is not a refusal: the kernel hides a
///    privilege-separated process's (`sshd: user@pts/N` is non-
///    dumpable), and the env is caller-choosable anyway — any agent
///    can scrub it before `exec` — so it only ever narrows;
/// 5. it holds no registered pane's pty on its stdio;
/// 6. its session leader is on that ancestry. A detach that orphans
///    (`setsid` then fork, the intermediate exits) leaves a session
///    whose leader is gone, and an orphan's origin is unknowable. A
///    session id of 0 (a leader outside this pid namespace) or 1
///    (init) cannot come from a detach inside it and passes.
///
/// Residual (CAD-280): the subreaper covers only trees this daemon
/// instance launched. A same-uid process started by a long-lived
/// process OUTSIDE them — a tmux server the daemon did not start (one
/// that outlived a daemon restart, the operator's, an agent-started
/// one), `systemd-run --user`, cron, `ssh localhost` — or detached
/// with `setsid -f` from a pane whose server is such a process, and
/// that scrubs its env and stdio, is its own session leader, is no
/// daemon descendant, and still passes: the "unattributable local
/// caller is the operator" weakness that operator-by-positive-proof
/// (CAD-280) designs out. Reconcile only frees holds the daemon itself
/// proves dead, so that residual cannot free work.
pub(crate) fn operator_proof(
    peer_pid: u32,
    uid: u32,
    daemon_pid: u32,
    panes: &HashMap<u32, String>,
    enrolled_root: impl Fn(u32) -> bool,
) -> Result<(), String> {
    let (real, effective) = proc_uids(peer_pid)?;
    if (real, effective) != (uid, uid) {
        return Err(format!(
            "pid {peer_pid} runs as uid {real}/{effective}, not the daemon's uid {uid}"
        ));
    }
    let chain = caller_chain(peer_pid)
        .ok_or_else(|| format!("pid {peer_pid}: /proc ancestry unreadable"))?;
    for (i, &hop) in chain.iter().enumerate() {
        if let Some(alias) = panes.get(&hop) {
            return Err(format!(
                "pid {hop} on its ancestry is registered pane '{alias}'"
            ));
        }
        if enrolled_root(hop) {
            return Err(format!(
                "pid {hop} on its ancestry is an enrolled managed endpoint"
            ));
        }
        if i > 0 && hop == daemon_pid {
            return Err(format!(
                "it descends from the daemon (pid {hop}) — a process the daemon launched"
            ));
        }
        let (real, effective) = proc_uids(hop)?;
        if real != uid && effective != uid {
            continue;
        }
        match std::fs::read(format!("/proc/{hop}/environ")) {
            Ok(env)
                if env
                    .split(|b| *b == 0)
                    .any(|kv| kv.starts_with(b"CADENCE_ALIAS=")) =>
            {
                return Err(format!(
                    "pid {hop} on its ancestry carries CADENCE_ALIAS — an agent's environment"
                ));
            }
            // A daemon-launched runner's tree (CAD-230b) carries its id:
            // a detached (`setsid -f`) descendant of a recipe leaves the
            // runner's ancestry but not its environment. One that also
            // scrubs the variable is still refused above, as a daemon
            // descendant — the daemon is its child subreaper (CAD-308).
            Ok(env)
                if env
                    .split(|b| *b == 0)
                    .any(|kv| kv.starts_with(b"CADENCE_RUNNER_ID=")) =>
            {
                return Err(format!(
                    "pid {hop} on its ancestry carries CADENCE_RUNNER_ID — a \
                     daemon-launched runner's environment"
                ));
            }
            Ok(_) => {}
            Err(e) if i == 0 => {
                return Err(format!("pid {peer_pid}: /proc/{peer_pid}/environ: {e}"));
            }
            Err(_) => {}
        }
    }
    if let Some(alias) = panes
        .iter()
        .find(|(pane_pid, _)| holds_pane_tty(peer_pid, **pane_pid))
        .map(|(_, alias)| alias)
    {
        return Err(format!(
            "it holds registered pane '{alias}''s pty on its stdio"
        ));
    }
    let sid = proc_session(peer_pid)?;
    if sid > 1 && !chain.contains(&sid) {
        return Err(format!(
            "its session leader pid {sid} is not on its ancestry — a detached \
             (setsid + fork) process, whose origin is unknowable"
        ));
    }
    Ok(())
}

/// Who is asking to change or delete an agent row — derived from the
/// connection, never from a request field (CAD-149, CAD-304 S3). The
/// daemon builds it from the peer's `/proc` ancestry (the nearest
/// registered pane or enrolled managed endpoint IS that agent) and,
/// deriving none, from [`operator_proof`]; anything else is refused
/// before a caller exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentCaller {
    /// Positive operator proof.
    Operator,
    /// A registered agent, by alias.
    Agent(String),
}

impl AgentCaller {
    /// `(by, by_kind)` for audit events.
    pub(crate) fn audit(&self) -> (&str, &'static str) {
        match self {
            AgentCaller::Operator => ("operator", "operator"),
            AgentCaller::Agent(alias) => (alias, "agent"),
        }
    }
}

/// What a mutation of an agent can do — the key class of the policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentMutation {
    /// Only self-service params (model, effort — see
    /// `registry::ParamClass::SelfService`).
    SelfService,
    /// Anything else: trust-bearing or posture params, and removal
    /// (which deletes the agent's history).
    Controlled,
}

/// The one rule for "may `caller` mutate agent `target`" (CAD-149,
/// shared with `agent remove`/`agent gc`, CAD-304 S3). `target_pm` is
/// the target's own PM — its `params.upstream`.
///
/// - the operator may do anything;
/// - the target's own PM may do anything to it (a PM is never its own
///   PM, even if an upstream names itself);
/// - the agent itself may make a self-service change only;
/// - everyone else — a peer worker, the PM of another group — is
///   refused.
///
/// `Err` is the refusal text, naming the rule.
pub(crate) fn may_mutate_agent(
    caller: &AgentCaller,
    target: &str,
    target_pm: Option<&str>,
    mutation: AgentMutation,
    verb: &str,
) -> Result<(), String> {
    let alias = match caller {
        AgentCaller::Operator => return Ok(()),
        AgentCaller::Agent(alias) => alias.as_str(),
    };
    if alias != target && target_pm == Some(alias) {
        return Ok(());
    }
    let owner = match target_pm.filter(|pm| *pm != target) {
        Some(pm) => format!("the operator or its PM '{pm}'"),
        None => "the operator (it has no PM)".to_string(),
    };
    if alias == target {
        return match mutation {
            AgentMutation::SelfService => Ok(()),
            AgentMutation::Controlled => Err(format!(
                "{verb} refused: agent '{alias}' cannot make this change to itself — \
                 an agent may set only its own model/effort; trust-bearing and \
                 posture params and removal of '{target}' belong to {owner} \
                 (caller rule, CAD-149)"
            )),
        };
    }
    Err(format!(
        "{verb} refused: agent '{alias}' cannot change another agent — only \
         '{target}' itself (model/effort) and {owner} may change '{target}' \
         (caller rule, CAD-149)"
    ))
}

/// Real and effective uid from `/proc/<pid>/status`.
pub(crate) fn proc_uids(pid: u32) -> Result<(u32, u32), String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .map_err(|e| format!("/proc/{pid}/status: {e}"))?;
    status
        .lines()
        .find_map(|l| {
            let mut ids = l.strip_prefix("Uid:")?.split_whitespace();
            Some((ids.next()?.parse().ok()?, ids.next()?.parse().ok()?))
        })
        .ok_or_else(|| format!("/proc/{pid}/status: no Uid line"))
}

/// The session id (`/proc/<pid>/stat` field 6).
fn proc_session(pid: u32) -> Result<u32, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|e| format!("/proc/{pid}/stat: {e}"))?;
    // comm (field 2) may hold spaces and parens — split after the last
    // ')': state is then index 0, and session (field 6) index 3.
    stat.rsplit_once(')')
        .and_then(|(_, after)| after.split_whitespace().nth(3)?.parse().ok())
        .ok_or_else(|| format!("/proc/{pid}/stat: malformed"))
}

/// `/proc/<pid>/stat` field 22 — the process start time in clock ticks
/// since boot. With the pid it is a process identity that survives pid
/// reuse: a later process holding the same pid has a later start.
/// `comm` (field 2) may hold spaces and parens, so fields are counted
/// after the LAST `)`: state (field 3) is index 0, field 22 index 19.
/// `None` when the process is gone or its `stat` is unreadable.
pub(crate) fn proc_starttime(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// What an agent row's recorded `(pid, start time)` proves about the
/// process holding that pid NOW (CAD-385). The pid alone never names an
/// agent: pids are reused, and a row outlives its process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PidProof {
    /// The pid still names the recorded process — same start time.
    Same,
    /// The pid names another process, or none: the row is stale and
    /// maps NOTHING — a caller reaching that pid is placed exactly as
    /// if the row had never been registered.
    Stale,
    /// Nothing to compare: a row recorded before CAD-385 (no start
    /// time), or a live process whose `stat` cannot be read. It may or
    /// may not be the row's process, so it fails closed: it vouches for
    /// no alias, and a caller reaching it is refused, never placed.
    Unproven,
}

/// Classify one recorded pid against `/proc` now ([`PidProof`]).
pub(crate) fn pid_proof(pid: u32, recorded_start: Option<u64>) -> PidProof {
    let Some(recorded) = recorded_start else {
        return PidProof::Unproven;
    };
    match proc_starttime(pid) {
        Some(now) if now == recorded => PidProof::Same,
        Some(_) => PidProof::Stale,
        // Gone: no process holds the pid, so no caller descends from
        // it. Present but unreadable: cannot tell.
        None if std::fs::metadata(format!("/proc/{pid}")).is_err() => PidProof::Stale,
        None => PidProof::Unproven,
    }
}

/// How a caller clears a row [`PidProof::Unproven`] — named in every
/// refusal and by `doctor --host` (check `pane-identity`).
pub(crate) const PID_START_REMEDY: &str = "restart the daemon on this build \
     (`cadence daemon restart`): recovery clears every recorded pid and each \
     live pane is adopted again with its process start time recorded; \
     `cadence doctor --host` (check pane-identity) lists the rows affected";

/// One agent's recorded process, classified.
#[derive(Debug, Clone)]
pub(crate) struct AgentPid {
    pub(crate) alias: String,
    pub(crate) pid: u32,
    pub(crate) proof: PidProof,
}

/// The agent rows' recorded processes — pane pids or managed provider
/// pids — each checked against its recorded start time (CAD-385).
/// [`PidProof::Stale`] rows are dropped at construction, so nothing
/// downstream can map a reused pid to the stale row's alias. Every
/// pid → alias mapping reads one of the views:
///
/// - [`Self::live`] — the rows whose process is proven: the only ones
///   that may IDENTIFY a caller as an agent;
/// - [`Self::fenced`] — live and unproven rows: deny lists (operator
///   proof, a pane answering its own menu), where a row that MAY still
///   be its process must keep refusing;
/// - [`Self::refuse_unproven_on`] — an unproven row on the caller's
///   ancestry refuses the call outright: it is neither that agent nor
///   provably anyone else.
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentPids {
    rows: Vec<AgentPid>,
}

impl AgentPids {
    /// Classify `(alias, pid, recorded start)` rows against `/proc` now.
    pub(crate) fn classify(rows: impl IntoIterator<Item = (String, u32, Option<u64>)>) -> Self {
        Self::from_proofs(
            rows.into_iter()
                .map(|(alias, pid, start)| (alias, pid, pid_proof(pid, start))),
        )
    }

    /// Rows already classified — tests, and callers with their own
    /// `/proc`.
    pub(crate) fn from_proofs(rows: impl IntoIterator<Item = (String, u32, PidProof)>) -> Self {
        Self {
            rows: rows
                .into_iter()
                .filter(|(_, _, proof)| *proof != PidProof::Stale)
                .map(|(alias, pid, proof)| AgentPid { alias, pid, proof })
                .collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn map(&self, keep: impl Fn(&AgentPid) -> bool) -> HashMap<u32, String> {
        self.rows
            .iter()
            .filter(|r| keep(r))
            .map(|r| (r.pid, r.alias.clone()))
            .collect()
    }

    /// pid → alias for rows whose recorded process is proven live.
    pub(crate) fn live(&self) -> HashMap<u32, String> {
        self.map(|r| r.proof == PidProof::Same)
    }

    /// pid → alias for every row that may still be its process.
    pub(crate) fn fenced(&self) -> HashMap<u32, String> {
        self.map(|_| true)
    }

    /// The row recorded for `alias`, when it is not stale.
    pub(crate) fn get(&self, alias: &str) -> Option<&AgentPid> {
        self.rows.iter().find(|r| r.alias == alias)
    }

    /// The unproven rows.
    pub(crate) fn unproven(&self) -> impl Iterator<Item = &AgentPid> {
        self.rows.iter().filter(|r| r.proof == PidProof::Unproven)
    }

    /// `Err` naming the nearest unproven row on `chain` (a caller's
    /// `/proc` ancestry, nearest first) and the remedy.
    pub(crate) fn refuse_unproven_on(&self, chain: &[u32]) -> Result<(), String> {
        let unproven = self.map(|r| r.proof == PidProof::Unproven);
        match chain
            .iter()
            .find_map(|pid| Some((*pid, unproven.get(pid)?)))
        {
            Some((pid, alias)) => Err(unproven_row(alias, pid)),
            None => Ok(()),
        }
    }
}

/// The refusal for a caller tied to an [`PidProof::Unproven`] row.
pub(crate) fn unproven_row(alias: &str, pid: u32) -> String {
    format!(
        "pid {pid} is agent '{alias}''s recorded process, but the row has no \
         process start time that proves it still is (recorded before CAD-385, \
         or /proc/{pid}/stat unreadable) — the pid may have been reused, so it \
         vouches for no one and caller identity is underivable; remedy: \
         {PID_START_REMEDY}"
    )
}

/// The live agents a board write can be attributed to, each by the
/// process the daemon recorded for it and checked against its recorded
/// start time ([`AgentPids`], CAD-385).
#[derive(Default)]
pub(crate) struct AgentRoots {
    /// Registered pty panes by pane pid. A peer is tied by ancestry or
    /// by the pane's pty on its stdio ([`PeerTies::attributed_agents`]).
    pub(crate) panes: AgentPids,
    /// Live managed endpoints (CAD-335) by provider pid: the
    /// claude/codex process the daemon launched with no pane, whose
    /// tool shells descend from it. A peer is tied by ancestry only —
    /// the provider's stdio is pipes and log files, never a pty.
    pub(crate) managed: AgentPids,
}

impl AgentRoots {
    pub(crate) fn is_empty(&self) -> bool {
        self.panes.is_empty() && self.managed.is_empty()
    }

    /// [`operator_proof`] for `pid` against these roots. The deny lists
    /// are every row that MAY still be its process (CAD-385,
    /// [`AgentPids::fenced`]): a reused pid denies nothing, a row with no
    /// recorded start keeps denying — panes and managed providers alike.
    /// The one way the board runs operator proof, so no call site builds
    /// a deny list from bare pids.
    pub(crate) fn operator_proof(&self, pid: u32, uid: u32, daemon_pid: u32) -> Result<(), String> {
        let panes = self.panes.fenced();
        let managed = self.managed.fenced();
        operator_proof(pid, uid, daemon_pid, &panes, |hop| {
            managed.contains_key(&hop)
        })
    }
}

/// Which live agent the TCP peer `peer` of a connection to our
/// `server_port` is attributed as: the registered pane on its ancestry
/// or whose pty it holds ([`PeerTies::attributed_agents`]), or the
/// managed endpoint whose provider is on its ancestry
/// ([`AgentRoots::managed`]) — each only while its recorded start time
/// still matches ([`AgentPids::live`], CAD-385): a row whose pid was
/// reused ties nothing. `Ok(None)` — the peer is a local process
/// tied to no agent, or (non-loopback address, no local socket holds
/// the connection's other end) a different host, which no agent here
/// can be. `Ok(None)` is NOT proof of the operator: a process that left
/// every agent's ancestry lands there too. `Err` — the peer could not
/// be attributed at all (unreadable ancestry, several agents, a tie to
/// a row whose process cannot be proven): callers must fail closed,
/// never read it as "no agent".
pub(crate) fn tcp_peer_agent(
    server_port: u16,
    peer: SocketAddr,
    roots: &AgentRoots,
) -> Result<Option<String>, String> {
    let peer = canonical(peer);
    let Some((inode, _)) = client_socket(server_port, peer)? else {
        if !peer.ip().is_loopback() {
            return Ok(None);
        }
        return Err(format!(
            "no local socket is the client end of {peer} → port {server_port}"
        ));
    };
    let pids = socket_owners(inode);
    if pids.is_empty() {
        return Err(format!(
            "socket {inode} of peer {peer} has no visible owner — another \
             user's process holds it, or it already closed"
        ));
    }
    let mut agents = BTreeSet::new();
    for pid in pids {
        let ties = PeerTies::probe(pid);
        if !ties.walked() {
            return Err(format!("peer pid {pid}: /proc ancestry unreadable"));
        }
        // A tie to a row whose process cannot be proven (CAD-385) is
        // neither that agent nor provably the operator: refuse.
        if let Some(row) = roots
            .panes
            .unproven()
            .find(|row| ties.process_tied(row.pid))
            .or_else(|| {
                roots
                    .managed
                    .unproven()
                    .find(|row| ties.descends_from(row.pid))
            })
        {
            return Err(unproven_row(&row.alias, row.pid));
        }
        let panes = roots.panes.live();
        agents.extend(
            ties.attributed_agents(panes.iter().map(|(pid, alias)| (alias.as_str(), *pid))),
        );
        agents.extend(
            roots
                .managed
                .live()
                .into_iter()
                .filter(|(provider, _)| ties.descends_from(*provider))
                .map(|(_, alias)| alias),
        );
    }
    if agents.len() > 1 {
        return Err(format!(
            "peer {peer} is tied to several agents ({})",
            agents.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(agents.into_iter().next())
}

/// Positive operator proof (CAD-276, [`operator_proof`]) for the TCP
/// peer of a connection to our `server_port` — the board's gate for
/// operator-only writes it relays to the daemon (CAD-328), which would
/// otherwise see only the board's own process. Every process holding
/// the client socket must pass: it runs as `uid`, walks cleanly, has
/// no registered pane or managed provider (`roots`, each unless its
/// recorded start time proves the pid reused — CAD-385) on its ancestry, is
/// no descendant of `daemon_pid` (under `daemon run` a detached child
/// of a daemon-launched tool re-parents to the daemon), carries no
/// agent environment, holds no pane pty, and leads or descends from
/// its session. A peer on another host, or whose socket has no visible
/// owner, is unprovable and refused.
pub(crate) fn tcp_peer_operator_proof(
    server_port: u16,
    peer: SocketAddr,
    uid: u32,
    daemon_pid: u32,
    roots: &AgentRoots,
) -> Result<(), String> {
    let peer = canonical(peer);
    let Some((inode, _)) = client_socket(server_port, peer)? else {
        return Err(format!(
            "no local socket is the client end of {peer} → port {server_port}"
        ));
    };
    let pids = socket_owners(inode);
    if pids.is_empty() {
        return Err(format!(
            "socket {inode} of peer {peer} has no visible owner"
        ));
    }
    for pid in pids {
        roots.operator_proof(pid, uid, daemon_pid)?;
    }
    Ok(())
}

/// A v4-mapped v6 address (`::ffff:127.0.0.1`, what a dual-stack
/// listener reports) compares as the v4 address the kernel lists.
pub(crate) fn canonical(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(IpAddr::V4(v4), addr.port()),
            None => addr,
        },
        IpAddr::V4(_) => addr,
    }
}

/// The client's half of the connection — the socket whose local end is
/// `peer` and whose remote end is our `server_port`, not the accepted
/// half this server holds: its inode and its owner uid (the uid column
/// the kernel prints for every socket, readable for another user's
/// socket too). `Ok(None)` when no table lists it; `Err` only when
/// neither table could be read.
pub(crate) fn client_socket(
    server_port: u16,
    peer: SocketAddr,
) -> Result<Option<(u64, u32)>, String> {
    let mut read_any = false;
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        read_any = true;
        if let Some(found) = find_client_socket(&text, server_port, peer) {
            return Ok(Some(found));
        }
    }
    if !read_any {
        return Err("/proc/net/tcp and /proc/net/tcp6 are unreadable".to_string());
    }
    Ok(None)
}

/// Scan one `/proc/net/tcp{,6}` table: `sl local rem st … uid timeout
/// inode`. Inode 0 (a TIME_WAIT remnant) is owned by nobody — skipped.
fn find_client_socket(table: &str, server_port: u16, peer: SocketAddr) -> Option<(u64, u32)> {
    table.lines().skip(1).find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let local = canonical(parse_addr(cols.get(1)?)?);
        let remote = canonical(parse_addr(cols.get(2)?)?);
        let uid: u32 = cols.get(7)?.parse().ok()?;
        let inode: u64 = cols.get(9)?.parse().ok()?;
        (local == peer && remote.port() == server_port && inode != 0).then_some((inode, uid))
    })
}

/// `0100007F:1F90` / 32-hex-digit v6 + `:PORT`. The kernel prints each
/// 32-bit address word in host byte order (`%08X` of the raw word), so
/// `to_ne_bytes` recovers the network-order bytes on any endianness;
/// the port is printed as a plain host-order number.
fn parse_addr(field: &str) -> Option<SocketAddr> {
    let (ip, port) = field.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let word = |i: usize| -> Option<[u8; 4]> {
        let hex = ip.get(i * 8..i * 8 + 8)?;
        Some(u32::from_str_radix(hex, 16).ok()?.to_ne_bytes())
    };
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(word(0)?)),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Every visible process holding `socket:[inode]` — more than one when
/// the socket was inherited across `fork`. Processes whose `fd` table
/// this user cannot read are skipped, never guessed.
fn socket_owners(inode: u64) -> Vec<u32> {
    let want = format!("socket:[{inode}]");
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut owners = Vec::new();
    for entry in procs.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        let holds = fds.flatten().any(|fd| {
            std::fs::read_link(fd.path()).is_ok_and(|target| target.as_os_str() == want.as_str())
        });
        if holds {
            owners.push(pid);
        }
    }
    owners
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    /// One row recorded the way the daemon records it: the pid with its
    /// start time read from `/proc` now.
    fn recorded(alias: &str, pid: u32) -> AgentPids {
        AgentPids::classify([(alias.to_string(), pid, proc_starttime(pid))])
    }

    #[test]
    fn parses_proc_net_addresses() {
        let v4 = u32::from_ne_bytes([127, 0, 0, 1]);
        let field = format!("{v4:08X}:1F90");
        assert_eq!(parse_addr(&field), Some("127.0.0.1:8080".parse().unwrap()));
        let mut v6 = String::new();
        let bytes = Ipv6Addr::LOCALHOST.octets();
        for chunk in bytes.chunks(4) {
            let word = u32::from_ne_bytes(chunk.try_into().unwrap());
            v6.push_str(&format!("{word:08X}"));
        }
        assert_eq!(
            parse_addr(&format!("{v6}:0050")),
            Some("[::1]:80".parse().unwrap())
        );
        assert_eq!(parse_addr("zz:0050"), None);
        let mapped: SocketAddr = "[::ffff:127.0.0.1]:5".parse().unwrap();
        assert_eq!(canonical(mapped), "127.0.0.1:5".parse().unwrap());
    }

    /// A live loopback connection resolves to this very process, and
    /// the pane map decides: our pid planted → that alias; no pane on
    /// our lineage → `None`.
    #[test]
    fn a_loopback_peer_resolves_to_its_process_and_pane() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        assert_eq!(peer, client.local_addr().unwrap());

        let none = AgentRoots::default();
        assert_eq!(tcp_peer_agent(port, peer, &none), Ok(None));
        let panes = AgentRoots {
            panes: recorded("w1", std::process::id()),
            ..Default::default()
        };
        assert_eq!(tcp_peer_agent(port, peer, &panes), Ok(Some("w1".into())));

        // A loopback address no local socket holds is not "no pane" —
        // it is unattributable, and the caller must refuse.
        let ghost: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(tcp_peer_agent(port, ghost, &panes).is_err());
        // A non-loopback address with no local client end is a
        // different host: no pane here can be it.
        let remote: SocketAddr = "192.0.2.7:40000".parse().unwrap();
        assert_eq!(tcp_peer_agent(port, remote, &panes), Ok(None));
    }

    /// CAD-335: a managed endpoint's provider on the peer's ancestry
    /// attributes the peer to that agent; one that is not on it (an
    /// unrelated live process) attributes nothing; a peer tied to a
    /// pane AND a managed provider is ambiguous and refused.
    #[test]
    fn a_loopback_peer_under_a_managed_provider_is_that_agent() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        let me = std::process::id();
        let parent = std::os::unix::process::parent_id();

        // Our parent stands in for the provider that launched us.
        let managed = AgentRoots {
            managed: recorded("wk", parent),
            ..Default::default()
        };
        assert_eq!(tcp_peer_agent(port, peer, &managed), Ok(Some("wk".into())));

        // A provider that is not on our ancestry: a child we spawned.
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let unrelated = AgentRoots {
            managed: recorded("wk", other.id()),
            ..Default::default()
        };
        assert_eq!(tcp_peer_agent(port, peer, &unrelated), Ok(None));
        let _ = other.kill();
        let _ = other.wait();

        let both = AgentRoots {
            panes: recorded("w1", me),
            managed: recorded("wk", parent),
        };
        assert!(tcp_peer_agent(port, peer, &both).is_err());
    }

    /// CAD-385: a recorded pid is the row's process only while its
    /// start time matches. A real unrelated process holding the pid
    /// with a different start is `Stale`, a gone one too; a row with no
    /// recorded start is `Unproven`, whatever holds the pid.
    #[test]
    fn pid_proof_tells_same_stale_and_unproven() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = proc_starttime(pid).unwrap();
        assert_eq!(pid_proof(pid, Some(start)), PidProof::Same);
        // The row recorded an EARLIER process under this pid.
        assert_eq!(pid_proof(pid, Some(start - 1)), PidProof::Stale);
        assert_eq!(pid_proof(pid, None), PidProof::Unproven);
        let rows = AgentPids::classify([
            ("live".to_string(), pid, Some(start)),
            ("reused".to_string(), pid, Some(start - 1)),
            ("legacy".to_string(), pid, None),
        ]);
        assert_eq!(rows.live(), HashMap::from([(pid, "live".to_string())]));
        assert!(rows.get("reused").is_none(), "a stale row maps nothing");
        assert_eq!(
            rows.unproven()
                .map(|r| r.alias.as_str())
                .collect::<Vec<_>>(),
            vec!["legacy"]
        );
        let err = rows.refuse_unproven_on(&[42, pid]).unwrap_err();
        assert!(
            err.contains("'legacy'") && err.contains("doctor --host"),
            "{err}"
        );
        assert!(rows.refuse_unproven_on(&[42]).is_ok());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(pid_proof(pid, Some(start)), PidProof::Stale, "gone");
    }

    /// CAD-385 on the board: a pane row whose pid now names another
    /// process (here: this test process, recorded with an earlier start)
    /// attributes the write to no agent — exactly as with no row; a row
    /// with no recorded start refuses the write and names the remedy.
    #[test]
    fn a_reused_or_unrecorded_pane_pid_never_attributes_a_board_write() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        let me = std::process::id();
        let start = proc_starttime(me).unwrap();
        let unregistered = tcp_peer_agent(port, peer, &AgentRoots::default());
        assert_eq!(unregistered, Ok(None));

        let reused = AgentRoots {
            panes: AgentPids::classify([("pm".to_string(), me, Some(start - 1))]),
            managed: AgentPids::classify([("wk".to_string(), me, Some(start + 1))]),
        };
        assert_eq!(tcp_peer_agent(port, peer, &reused), unregistered);

        let legacy = AgentRoots {
            panes: AgentPids::classify([("pm".to_string(), me, None)]),
            ..Default::default()
        };
        let err = tcp_peer_agent(port, peer, &legacy).unwrap_err();
        assert!(
            err.contains("'pm'") && err.contains("doctor --host"),
            "{err}"
        );
        let legacy_managed = AgentRoots {
            managed: AgentPids::classify([("wk".to_string(), me, None)]),
            ..Default::default()
        };
        assert!(tcp_peer_agent(port, peer, &legacy_managed).is_err());
    }

    /// CAD-385 in the board's operator proof (CAD-328): the pane and
    /// managed deny lists are the fenced views — a row whose pid now
    /// names another process denies nothing (the answer is exactly the
    /// unregistered one), while a live row and a row with no recorded
    /// start both still deny.
    #[test]
    fn board_operator_proof_denies_live_and_unproven_rows_but_not_reused_ones() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        let me = std::process::id();
        let start = proc_starttime(me).unwrap();
        let (_, uid) = proc_uids(me).unwrap();
        let proof = |roots: &AgentRoots| tcp_peer_operator_proof(port, peer, uid, 0, roots);
        let unregistered = proof(&AgentRoots::default());

        let reused = AgentRoots {
            panes: AgentPids::classify([("pm".to_string(), me, Some(start - 1))]),
            managed: AgentPids::classify([("wk".to_string(), me, Some(start + 1))]),
        };
        assert_eq!(proof(&reused), unregistered);

        for start in [Some(start), None] {
            let pane = AgentRoots {
                panes: AgentPids::classify([("pm".to_string(), me, start)]),
                ..Default::default()
            };
            let err = proof(&pane).unwrap_err();
            assert!(err.contains("registered pane 'pm'"), "{err}");
            let managed = AgentRoots {
                managed: AgentPids::classify([("wk".to_string(), me, start)]),
                ..Default::default()
            };
            let err = proof(&managed).unwrap_err();
            assert!(err.contains("enrolled managed endpoint"), "{err}");
        }
    }

    /// The client socket's uid column is read, and it is ours for a
    /// connection this process opened.
    #[test]
    fn a_loopback_client_socket_carries_its_owner_uid() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_accepted, peer) = listener.accept().unwrap();
        let (_, own) = proc_uids(std::process::id()).unwrap();
        let (_, uid) = client_socket(port, peer).unwrap().unwrap();
        assert_eq!(uid, own);
    }

    /// CAD-149: the policy per caller kind and mutation class.
    #[test]
    fn may_mutate_agent_per_caller_kind() {
        use AgentMutation::{Controlled, SelfService};
        let op = AgentCaller::Operator;
        let pm = AgentCaller::Agent("pm".into());
        let pm2 = AgentCaller::Agent("pm2".into());
        let w1 = AgentCaller::Agent("w1".into());
        let w2 = AgentCaller::Agent("w2".into());
        let may = |c: &AgentCaller, target: &str, target_pm: Option<&str>, m| {
            may_mutate_agent(c, target, target_pm, m, "agent set")
        };
        for m in [SelfService, Controlled] {
            // Operator: everything, PM or not.
            assert!(may(&op, "w2", Some("pm"), m).is_ok());
            assert!(may(&op, "root", None, m).is_ok());
            // The target's own PM: everything.
            assert!(may(&pm, "w2", Some("pm"), m).is_ok());
            // A peer worker and another group's PM: nothing.
            let e = may(&w1, "w2", Some("pm"), m).unwrap_err();
            assert!(e.contains("cannot change another agent"), "{e}");
            assert!(e.contains("its PM 'pm'"), "{e}");
            assert!(may(&pm2, "w2", Some("pm"), m).is_err());
            // A worker that happens to be named as nothing: refused.
            assert!(may(&w2, "w1", None, m).is_err());
        }
        // Self: self-service only — trust/posture/removal refused, and
        // the refusal names the rule.
        assert!(may(&w1, "w1", Some("pm"), SelfService).is_ok());
        let e = may(&w1, "w1", Some("pm"), Controlled).unwrap_err();
        assert!(e.contains("cannot make this change to itself"), "{e}");
        assert!(e.contains("caller rule"), "{e}");
        // A PM on itself is not its own PM.
        assert!(may(&pm, "pm", None, Controlled).is_err());
        assert!(may(&pm, "pm", None, SelfService).is_ok());
        // An upstream naming the target itself grants it nothing.
        assert!(may(&w1, "w1", Some("w1"), Controlled).is_err());
    }
}
