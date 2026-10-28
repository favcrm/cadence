//! Retrieval matching and freshness labelling: which accepted memories
//! apply to a dispatch context, how they rank, and how their evidence
//! is labelled or withheld. Scope globs, the freshness window and the
//! validity-withhold policy all live here; native policy qualification stays
//! in the parent — `match_memories` still calls `retrieval_status`.
//!
//! Two path axes reach a lesson: concrete paths already in the context
//! (recorded commits) through `glob_match`, and an issue's declared
//! planned paths — directories and globs included — as the set of files
//! they cover (`Planned::reaches`). The planned axis only ever adds
//! selections inside the lesson's own scope; it never widens a lesson
//! past what a commit of the same file would select.

use std::collections::VecDeque;
use std::path::Path;

use serde_json::{json, Value};

use crate::issue::{project, time};

use super::{retrieval_status, Memory, Scope};

/// What a dispatch/match call knows about the target.
#[derive(Clone, Debug, Default)]
pub struct MatchCtx {
    pub components: Vec<String>,
    /// Concrete repo-relative paths the issue is likely to touch.
    pub paths: Vec<String>,
    pub providers: Vec<String>,
    pub tags: Vec<String>,
    /// Native assigned task/team roles; unknown is an empty set.
    pub roles: Vec<String>,
}

/// `*` within a path segment, `**` across segments, `?` one char.
/// Unanchored suffixes (`src/**` alone) still require the leading
/// segments to match — globs are anchored at both ends.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn inner(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        if p.starts_with(b"**") {
            let rest = &p[2..];
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            // `**` spans slashes: try every split point of the string.
            return (0..=s.len()).any(|i| inner(rest, &s[i..]));
        }
        match p[0] {
            b'*' => {
                for i in 0..=s.len() {
                    if inner(&p[1..], &s[i..]) {
                        return true;
                    }
                    if i == s.len() || s[i] == b'/' {
                        return false;
                    }
                }
                false
            }
            b'?' => !s.is_empty() && s[0] != b'/' && inner(&p[1..], &s[1..]),
            c => !s.is_empty() && s[0] == c && inner(&p[1..], &s[1..]),
        }
    }
    inner(pattern.as_bytes(), path.as_bytes())
}

/// One byte a path step consumes: any byte (`**` in a lesson scope), any
/// byte but `/` (inside one path segment), or one literal byte.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Any,
    InSegment,
    Lit(u8),
}

/// Can one byte satisfy both steps?
fn shares(a: Step, b: Step) -> bool {
    match (a, b) {
        (Step::Lit(x), Step::Lit(y)) => x == y,
        (Step::Lit(x), Step::InSegment) | (Step::InSegment, Step::Lit(x)) => x != b'/',
        _ => true,
    }
}

fn is_wild(segment: &str) -> bool {
    segment.contains('*') || segment.contains('?')
}

/// The segments a planned declaration denotes, mirroring `issue::areas`
/// (`segments` plus its wildcard-free subtree rule): a trailing `/` or a
/// wildcard-free path also covers everything under it, so either gains a
/// trailing `**`, which matches zero segments — the path itself.
fn planned_segments(decl: &str) -> Vec<&str> {
    let body = decl.strip_suffix('/').unwrap_or(decl);
    let mut segments: Vec<&str> = body.split('/').collect();
    if decl.ends_with('/') || !segments.iter().any(|s| is_wild(s)) {
        segments.push("**");
    }
    segments
}

/// The byte move out of `at` in the lesson scope's `glob_match`
/// automaton — the same three rules that function applies, so the
/// automaton accepts exactly the paths it accepts.
fn scope_byte(pattern: &[u8], at: usize) -> Option<(Step, usize)> {
    if pattern[at..].starts_with(b"**") {
        return Some((Step::Any, at));
    }
    match pattern.get(at) {
        Some(b'*') => Some((Step::InSegment, at)),
        Some(b'?') => Some((Step::InSegment, at + 1)),
        Some(&c) => Some((Step::Lit(c), at + 1)),
        None => None,
    }
}

/// The epsilon moves out of `at`: `**` may consume nothing and absorbs
/// one following `/` (exactly as `glob_match` strips it), `*` may match
/// nothing.
fn scope_eps(pattern: &[u8], at: usize) -> [Option<usize>; 2] {
    if pattern[at..].starts_with(b"**") {
        let past = at + 2;
        let slash = if pattern.get(past) == Some(&b'/') {
            Some(past + 1)
        } else {
            None
        };
        [Some(past), slash]
    } else if pattern.get(at) == Some(&b'*') {
        [Some(at + 1), None]
    } else {
        [None, None]
    }
}

/// A planned declaration's automaton over the `/`-prefixed path. Every
/// segment is `/`-prefixed there, so `**` (zero or more whole segments)
/// needs no first-segment special case and no segment can be empty; a
/// real path is the only thing it accepts. Built once per declaration.
struct Planned<'a> {
    /// The declaration itself, for the concrete-scope fast path.
    decl: &'a str,
    byte: Vec<Option<(Step, usize)>>,
    eps: Vec<Option<usize>>,
    start: usize,
    accept: usize,
}

impl<'a> Planned<'a> {
    /// `decl` is a validated declaration (`issue::areas::check_path`).
    fn build(decl: &'a str) -> Self {
        let segments = planned_segments(decl);
        let mut base = Vec::with_capacity(segments.len());
        let mut states = 0;
        for segment in &segments {
            base.push(states);
            states += if *segment == "**" {
                3
            } else {
                1 + 2 * (segment.len() + 1)
            };
        }
        let accept = states;
        states += 1;
        let mut byte = vec![None; states];
        let mut eps = vec![None; states];
        let entry = |k: usize| if k < segments.len() { base[k] } else { accept };
        for (k, segment) in segments.iter().enumerate() {
            let next = entry(k + 1);
            if *segment == "**" {
                // open: zero segments, or another `/segment`; need: that
                // `/` must be followed by a byte; run: the segment body.
                let (open, need, run) = (base[k], base[k] + 1, base[k] + 2);
                eps[open] = Some(next);
                byte[open] = Some((Step::Lit(b'/'), need));
                byte[need] = Some((Step::InSegment, run));
                byte[run] = Some((Step::InSegment, run));
                eps[run] = Some(open);
            } else {
                let (head, body) = (base[k], base[k] + 1);
                byte[head] = Some((Step::Lit(b'/'), body));
                for at in 0..=segment.len() {
                    for started in [false, true] {
                        let state = body + at * 2 + started as usize;
                        let Some(&c) = segment.as_bytes().get(at) else {
                            if started {
                                eps[state] = Some(next);
                            }
                            continue;
                        };
                        let (step, to) = match c {
                            b'*' => (Step::InSegment, body + at * 2 + 1),
                            b'?' => (Step::InSegment, body + (at + 1) * 2 + 1),
                            _ => (Step::Lit(c), body + (at + 1) * 2 + 1),
                        };
                        byte[state] = Some((step, to));
                        if c == b'*' {
                            eps[state] = Some(body + (at + 1) * 2 + started as usize);
                        }
                    }
                }
            }
        }
        Planned {
            decl,
            byte,
            eps,
            start: entry(0),
            accept,
        }
    }

    /// Does lesson scope glob `scope` reach a file this declaration
    /// covers?
    ///
    /// A scope naming exactly one file is the membership question the
    /// native declaration grammar already answers. Otherwise the two
    /// automata are intersected one byte at a time, which is exact for
    /// both grammars: the scope keeps `glob_match`'s reach (a `**` inside
    /// a segment still spans separators) and the declaration admits only
    /// real paths. Bounded by (|scope| + 2) × states, deterministic, no
    /// recursion.
    fn reaches(&self, scope: &str) -> bool {
        if !is_wild(scope) && !scope.split('/').any(str::is_empty) {
            return crate::issue::areas::matches(self.decl, scope);
        }
        let mut prefixed = String::with_capacity(scope.len() + 1);
        prefixed.push('/');
        prefixed.push_str(scope);
        let pattern = prefixed.as_bytes();
        let states = self.byte.len();
        let mut seen = vec![false; (pattern.len() + 1) * states];
        let mut queue = VecDeque::from([(0usize, self.start)]);
        seen[self.start] = true;
        fn push(
            queue: &mut VecDeque<(usize, usize)>,
            seen: &mut [bool],
            states: usize,
            at: usize,
            state: usize,
        ) {
            let slot = at * states + state;
            if !seen[slot] {
                seen[slot] = true;
                queue.push_back((at, state));
            }
        }
        while let Some((at, state)) = queue.pop_front() {
            if at == pattern.len() && state == self.accept {
                return true;
            }
            for eps in scope_eps(pattern, at).into_iter().flatten() {
                push(&mut queue, &mut seen, states, eps, state);
            }
            if let Some(next) = self.eps[state] {
                push(&mut queue, &mut seen, states, at, next);
            }
            if let (Some((want, to_at)), Some((can, to_state))) =
                (scope_byte(pattern, at), self.byte[state])
            {
                if shares(want, can) {
                    push(&mut queue, &mut seen, states, to_at, to_state);
                }
            }
        }
        false
    }
}

/// Union semantics per the kickoff: a memory applies when ANY scope
/// axis matches the context. Declared planned paths are a path axis too,
/// matched as the set of files a declaration covers — never as one
/// string — so a directory or glob declaration still selects the lesson.
fn applies(scope: &Scope, ctx: &MatchCtx, planned: &[Planned]) -> bool {
    scope.project
        || scope.components.iter().any(|c| ctx.components.contains(c))
        || scope.paths.iter().any(|g| {
            ctx.paths.iter().any(|p| glob_match(g, p)) || planned.iter().any(|decl| decl.reaches(g))
        })
        || scope.tags.iter().any(|t| ctx.tags.contains(t))
        || scope.providers.iter().any(|p| ctx.providers.contains(p))
}

fn type_rank(kind: &str) -> u8 {
    match kind {
        "rule" => 0,
        "gotcha" => 1,
        "recipe" => 2,
        _ => 3,
    }
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    }
}

/// Default evidence freshness window in days. A project overrides it
/// with `memory: {stale_days: N}` in its project.yaml.
pub const DEFAULT_STALE_DAYS: u64 = 30;

/// Retrieval-time freshness policy: the window a verify stays current
/// and the clock it is measured against (explicit so tests can pin time).
#[derive(Clone, Copy, Debug)]
pub struct Freshness {
    pub window_days: u64,
    pub now: i64,
}

impl Freshness {
    /// The project's window (or the default) measured from now.
    pub fn for_project(proj: Option<&project::Project>) -> Self {
        Freshness {
            window_days: proj
                .and_then(|p| p.memory.as_ref())
                .and_then(|m| m.stale_days)
                .unwrap_or(DEFAULT_STALE_DAYS),
            now: time::now_epoch(),
        }
    }

    /// `for_project` by key — an unknown project gets the default.
    pub fn for_key(pm_dir: &Path, key: &str) -> Self {
        Freshness::among(&project::list(pm_dir).unwrap_or_default(), key)
    }

    /// `for_key` over an already-listed registry — one read for a list
    /// of lessons that spans projects.
    pub fn among(projects: &[project::Project], key: &str) -> Self {
        Freshness::for_project(projects.iter().find(|p| p.key == key))
    }

    /// Whether a verify at `at` is still inside the window. An unreadable
    /// time is not current — it can only decay the label, never withhold.
    fn current(&self, at: &str) -> bool {
        iso_epoch(at).is_some_and(|epoch| {
            self.now.saturating_sub(epoch) <= (self.window_days as i64).saturating_mul(86400)
        })
    }
}

/// When the lesson's evidence was last re-checked: the PM finalization
/// of its current verify cycle. Acceptance is a review, not a re-check,
/// so an accepted lesson with no verify finalization is unverified —
/// including legacy records whose `verified_at` was stamped at accept.
pub fn last_verified(mem: &Memory) -> Option<&str> {
    if mem.front.policy.is_some() {
        // Policy review/source attestation is not a CAD-348 check receipt.
        return None;
    }
    if mem.front.review_cycle < 2 {
        return None;
    }
    mem.front
        .finalizations
        .iter()
        .find(|r| r.operation == "verify" && r.cycle == mem.front.review_cycle)
        .map(|r| r.finalized_at.as_str())
}

/// The last verify, only while it is inside the freshness window.
pub(super) fn current_verify<'a>(mem: &'a Memory, fresh: &Freshness) -> Option<&'a str> {
    last_verified(mem).filter(|at| fresh.current(at))
}

/// The evidence label an injected lesson carries — never presented as
/// verified without a current verify cycle: `verified <date>` inside the
/// window, `unverified (last verified <date>)` once it has decayed past
/// it, `unverified` when it was never verified.
pub fn evidence_label(mem: &Memory, fresh: &Freshness) -> String {
    let day = |at: &str| at.get(..10).unwrap_or(at).to_string();
    match last_verified(mem) {
        Some(at) if fresh.current(at) => format!("verified {}", day(at)),
        Some(at) => format!("unverified (last verified {})", day(at)),
        None => "unverified".to_string(),
    }
}

/// Why a lesson is withheld: its evidence is explicitly marked stale
/// (the `stale:` mark; later the CAD-111 citation re-check). Age alone
/// never withholds — a lesson past the window decays to `unverified` and
/// is still injected, labelled (`evidence_label`).
pub fn stale_reason(mem: &Memory) -> Option<String> {
    let why = mem.front.stale.as_deref()?.trim();
    Some(if why.is_empty() {
        "evidence marked stale".to_string()
    } else {
        format!("evidence marked stale: {why}")
    })
}

pub(super) fn withhold_reason(mem: &Memory, fresh: &Freshness) -> Option<String> {
    stale_reason(mem).or_else(|| super::v2::unused_reason(mem, fresh.now))
}

/// A lesson's evidence as retrieval reads it — the one view dispatch,
/// the board and `memory ls --stale` share: `withheld` with its reason
/// when explicitly marked stale, else the `evidence_label` (`verified`
/// only inside the window). `last_verified` is the verify finalization,
/// never the raw `verified_at` field.
pub fn evidence_json(mem: &Memory, fresh: &Freshness) -> Value {
    let (state, label, reason) = match withhold_reason(mem, fresh) {
        Some(reason) => ("withheld", "withheld".to_string(), Some(reason)),
        None if current_verify(mem, fresh).is_some() => {
            ("verified", evidence_label(mem, fresh), None)
        }
        None => ("unverified", evidence_label(mem, fresh), None),
    };
    json!({
        "state": state,
        "label": label,
        "reason": reason,
        "last_verified": last_verified(mem),
        "window_days": fresh.window_days,
    })
}

/// A match result: what may be injected and what was withheld, plus the
/// freshness policy that labels the injected lessons.
#[derive(Clone, Debug)]
pub struct Matched {
    /// Injectable lessons, ranked.
    pub lessons: Vec<Memory>,
    /// Policy-qualified lessons withheld for validity or unknown target role,
    /// with the reason needed to explain absent guidance.
    pub withheld: Vec<(Memory, String)>,
    pub fresh: Freshness,
}

impl Matched {
    /// The evidence label for one of `lessons`.
    pub fn label(&self, mem: &Memory) -> String {
        evidence_label(mem, &self.fresh)
    }
}

/// Accepted memories that apply to `ctx`, ranked: type
/// (rule>gotcha>recipe>decision), confidence (high first), most recently
/// verified first (unverified and decayed last). Explicitly stale ones
/// are withheld with a reason.
pub fn match_memories(memories: &[Memory], ctx: &MatchCtx, fresh: &Freshness) -> Matched {
    ranked(memories, ctx, fresh, &[])
}

/// `match_memories` plus the target's declared planned paths: validated
/// repo-relative files or globs an issue plans to touch, matched before
/// any commit exists. Every other axis, the ranking and the withholding
/// policy are exactly `match_memories`; a declaration only ever reaches
/// a lesson through that lesson's own path scope.
pub fn match_memories_planned(
    memories: &[Memory],
    ctx: &MatchCtx,
    fresh: &Freshness,
    planned: &[String],
) -> Matched {
    ranked(memories, ctx, fresh, planned)
}

fn ranked(memories: &[Memory], ctx: &MatchCtx, fresh: &Freshness, planned: &[String]) -> Matched {
    // One automaton per declaration, shared by every candidate lesson.
    let planned: Vec<Planned> = planned.iter().map(|decl| Planned::build(decl)).collect();
    let mut out = Matched {
        lessons: Vec::new(),
        withheld: Vec::new(),
        fresh: *fresh,
    };
    for m in memories {
        if m.front.status != "accepted"
            || !retrieval_status(m).0
            || !applies(&m.front.scope, ctx, &planned)
        {
            continue;
        }
        if !super::v2::role_matches(m, ctx.roles.iter().map(String::as_str)) {
            if ctx.roles.is_empty() {
                out.withheld.push((
                    m.clone(),
                    "native target role is unavailable for role-scoped guidance".to_owned(),
                ));
            }
            continue;
        }
        match withhold_reason(m, fresh) {
            Some(reason) => out.withheld.push((m.clone(), reason)),
            None => out.lessons.push(m.clone()),
        }
    }
    out.lessons.sort_by(|a, b| {
        (
            type_rank(&a.front.kind),
            confidence_rank(&a.front.confidence),
            current_verify(b, fresh).unwrap_or_default(),
            &a.front.id,
        )
            .cmp(&(
                type_rank(&b.front.kind),
                confidence_rank(&b.front.confidence),
                current_verify(a, fresh).unwrap_or_default(),
                &b.front.id,
            ))
    });
    out
}

/// `YYYY-MM-DD[THH:MM:SSZ]` → epoch seconds; needed to bound the
/// staleness window without chrono. Byte-sliced — a non-ASCII or
/// short timestamp is "unknown" (None), never a panic.
pub(super) fn iso_epoch(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let n = |i: usize, j: usize| -> Option<i64> {
        let slice = b.get(i..j)?;
        if slice.iter().all(|c| c.is_ascii_digit()) {
            std::str::from_utf8(slice).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss) = if b.len() >= 19 {
        (n(11, 13)?, n(14, 16)?, n(17, 19)?)
    } else {
        (0, 0, 0)
    };
    // days-from-civil (Howard Hinnant) → seconds.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}
