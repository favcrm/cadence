//! Bounded overview source acquisition, including borrowed board reuse.
//! Row assembly and its contextual reads live separately in assembly.rs.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

mod probe;

use super::github_cache::github_throttled;
use super::{github_repos_cached, monitoring, now_epoch, Options};
use crate::issue::{self, board, project};
use probe::daemon_view;
#[cfg(test)]
pub(super) use probe::probe_agent;
pub(super) use probe::{AgentProbe, DaemonView};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The daemon-side sources of one build — the agent probes and the
/// monitoring block. The board's read model (CAD-325) keeps the last
/// pass and rebuilds the screen from it when only the tracker moved.
#[derive(Clone)]
pub struct DaemonSources {
    pub(super) daemon: DaemonView,
    pub(super) monitoring: Value,
}

/// Probe the daemon and read the monitors, concurrently.
pub fn daemon_sources(state_dir: &Path, opts: &Options) -> DaemonSources {
    std::thread::scope(|s| {
        let daemon = s.spawn(|| daemon_view(state_dir, opts));
        let monitoring = s.spawn(|| monitoring(state_dir));
        DaemonSources {
            daemon: daemon.join().expect("overview daemon probe panicked"),
            monitoring: monitoring
                .join()
                .expect("overview monitoring read panicked"),
        }
    })
}

/// What a long-lived caller hands a build instead of re-reading it: the
/// indexed tracker's views and a recent daemon pass. Status and claim
/// times need no hand-off — [`crate::issue::line_times::LineTimes`] is cached per tracker HEAD
/// (CAD-403).
pub struct Reuse<'a> {
    pub views: &'a [board::View],
    pub sources: &'a DaemonSources,
}

/// Inputs collected once for one build. Reused daemon/tracker results stay borrowed.
pub(super) struct Snapshot<'a> {
    pub(super) now: i64,
    pub(super) pm: Option<issue::Pm>,
    pub(super) projects: Vec<project::Project>,
    pub(super) repo_paths: Vec<(PathBuf, String)>,
    /// GitHub slug → every project claiming it, sorted. One entry is
    /// attribution; several mean one repository serves several projects
    /// (CAD-2001), and a row on it is attributed only when a branch or an
    /// issue names one — never the last claimant.
    pub(super) slug_project: HashMap<String, Vec<String>>,
    pub(super) slug_clone: HashMap<String, PathBuf>,
    pub(super) has_github: bool,
    pub(super) sources: Cow<'a, DaemonSources>,
    pub(super) views: Option<Cow<'a, [board::View]>>,
    pub(super) gh_repos: HashMap<String, Value>,
    pub(super) gh_state: Value,
}

pub(super) fn collect<'a>(
    state_dir: &Path,
    pm_dir: &Path,
    opts: &Options,
    reuse: Option<Reuse<'a>>,
) -> Result<Snapshot<'a>, String> {
    let now = now_epoch();
    let pm = issue::Pm::at(pm_dir).ok();
    let projects = pm
        .as_ref()
        .map(|pm| project::list(&pm.dir).unwrap_or_default())
        .unwrap_or_default();
    if let Some(key) = opts.scope.project.as_deref() {
        if !projects.iter().any(|p| p.key == key) {
            let known: Vec<&str> = projects.iter().map(|p| p.key.as_str()).collect();
            return Err(if known.is_empty() {
                format!(
                    "unknown project '{key}' — no tracker projects under {}",
                    pm_dir.display()
                )
            } else {
                format!("unknown project '{key}' — known: {}", known.join(", "))
            });
        }
    }
    let mut slugs = Vec::new();
    let mut slug_project: HashMap<String, Vec<String>> = HashMap::new();
    let mut repo_paths: Vec<(PathBuf, String)> = Vec::new();
    // Slug → the declared local clone its first-parent log comes from.
    let mut slug_clone: HashMap<String, PathBuf> = HashMap::new();
    for p in &projects {
        for r in &p.repos {
            let path = r.path.as_deref().map(|path| {
                let path = project::expand_home(path);
                path.canonicalize().unwrap_or(path)
            });
            if let Some(remote) = &r.remote {
                let norm = project::normalize_remote(remote);
                if let Some(slug) = norm.strip_prefix("github.com/") {
                    // One repo, one project is the common case; a repo
                    // several projects register keeps every claimant so
                    // nothing is silently attributed to the last one.
                    let claims = slug_project.entry(slug.to_string()).or_default();
                    if !claims.iter().any(|key| key == &p.key) {
                        claims.push(p.key.clone());
                    }
                    slugs.push(slug.to_string());
                    if let Some(path) = &path {
                        slug_clone
                            .entry(slug.to_string())
                            .or_insert_with(|| path.clone());
                    }
                }
            }
            if let Some(path) = path {
                repo_paths.push((path, p.key.clone()));
            }
        }
    }
    slugs.sort();
    slugs.dedup();
    for claims in slug_project.values_mut() {
        claims.sort();
    }

    // ---- the three sources, concurrently (what `reuse` lacks) ----
    let reused = reuse.as_ref();
    let (fresh_sources, (gh_repos, gh_state), fresh_views) = std::thread::scope(|s| {
        let sources = reused
            .is_none()
            .then(|| s.spawn(|| daemon_sources(state_dir, opts)));
        let gh = s.spawn(|| {
            if opts.cache_only {
                github_repos_cached(state_dir, &slugs)
            } else {
                github_throttled(state_dir, &slugs, opts.gh_wait, opts.gh_cache_secs)
            }
        });
        // Local files only — the notes index keeps it one pass.
        let tracker = reused.is_none().then(|| {
            s.spawn(|| {
                pm.as_ref().map(|pm| {
                    let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                    board::views(&pm.config.notes_dir(), issues)
                })
            })
        });
        (
            sources.map(|h| h.join().expect("overview daemon sources panicked")),
            gh.join().expect("overview gh refresh panicked"),
            tracker.and_then(|h| h.join().expect("overview tracker read panicked")),
        )
    });
    let sources = match (reused, fresh_sources) {
        (Some(r), _) => Cow::Borrowed(r.sources),
        (None, Some(fresh)) => Cow::Owned(fresh),
        (None, None) => unreachable!("sources are gathered unless reused"),
    };
    let views = match reused {
        Some(r) => pm.as_ref().map(|_| Cow::Borrowed(r.views)),
        None => fresh_views.map(Cow::Owned),
    };
    Ok(Snapshot {
        now,
        pm,
        projects,
        repo_paths,
        slug_project,
        slug_clone,
        has_github: !slugs.is_empty(),
        sources,
        views,
        gh_repos,
        gh_state,
    })
}
