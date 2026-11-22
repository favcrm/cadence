//! Scope glob matching and planned-path automaton intersection for memory retrieval.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::VecDeque;

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
pub(super) struct Planned<'a> {
    /// The declaration itself, for the concrete-scope fast path.
    decl: &'a str,
    byte: Vec<Option<(Step, usize)>>,
    eps: Vec<Option<usize>>,
    start: usize,
    accept: usize,
}

impl<'a> Planned<'a> {
    /// `decl` is a validated declaration (`issue::areas::check_path`).
    pub(super) fn build(decl: &'a str) -> Self {
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
    pub(super) fn reaches(&self, scope: &str) -> bool {
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
