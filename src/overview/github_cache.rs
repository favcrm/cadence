//! The Overview GitHub display cache and bounded refresh lifecycle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::{gh_repo, now_epoch, Options, GH_CACHE_MAX_SECS, GH_CACHE_SECS};

pub(super) fn cache_file(state_dir: &Path) -> PathBuf {
    state_dir.join("overview-gh.json")
}

/// The cache body on disk: the slug set the rows were fetched for
/// plus the repo payloads. A body only serves a request for the same
/// slug set — a tracker with no GitHub remotes must not blank the
/// board's rows, and a different tracker must not inherit them.
#[derive(Clone)]
pub(super) struct GhCache {
    pub(super) at: i64,
    pub(super) slugs: Vec<String>,
    pub(super) repos: HashMap<String, Value>,
}

pub(super) fn read_cache(file: &Path) -> Option<GhCache> {
    let text = std::fs::read_to_string(file).ok()?;
    let cached: Value = serde_json::from_str(&text).ok()?;
    let at = cached["at"].as_i64()?;
    let slugs = cached["slugs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let repos = cached["repos"]
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    Some(GhCache { at, slugs, repos })
}

/// Temp-write then rename — a crashed reader never sees half a body.
pub(super) fn write_cache(file: &Path, slugs: &[String], repos: &HashMap<String, Value>, at: i64) {
    let tmp = file.with_extension("tmp");
    let body = serde_json::to_string(&json!({
        "at": at, "slugs": slugs, "repos": repos,
    }))
    .unwrap_or_default();
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
}

/// One repo's `gh` read — [`gh_repo`] in production, a stub in tests.
pub(super) type GhFetch = fn(&str) -> Result<Value, String>;

/// gh refreshes in flight in this process, by cache file: while one
/// runs, other requests serve the cache instead of starting another.
static GH_REFRESHING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The GitHub block, 60 s-cached under the state dir, waiting as long
/// as the one-shot CLI needs.
pub(super) fn github(state_dir: &Path, slugs: &[String]) -> (HashMap<String, Value>, Value) {
    let opts = Options::cli();
    // `session` shares this cache, but keeps its existing freshness bound.
    // The configurable display age applies only in `overview_from`.
    github_bounded(state_dir, slugs, opts.gh_wait, GH_CACHE_SECS, gh_repo)
}

/// The GitHub block with a bounded wait (CAD-249). Returns the repos
/// map plus `{state: ok|cached|stale|unavailable, as_of, error?}` —
/// `as_of` is when the rows were fetched. A fresh cache answers at
/// once; otherwise a refresh starts (every slug concurrently) and the
/// caller waits at most `wait` for it. Past that, the last good body
/// for this slug set is served as `stale` while the refresh finishes
/// in the background and lands in the cache for the next request.
pub(super) fn github_bounded(
    state_dir: &Path,
    slugs: &[String],
    wait: Duration,
    cache_secs: i64,
    fetch: GhFetch,
) -> (HashMap<String, Value>, Value) {
    let file = cache_file(state_dir);
    let now = now_epoch();
    let cached = read_cache(&file);
    if let Some(c) = &cached {
        if now - c.at < cache_secs.clamp(GH_CACHE_SECS, GH_CACHE_MAX_SECS) && c.slugs == slugs {
            return (
                c.repos.clone(),
                json!({"state": "cached", "at": c.at, "as_of": c.at}),
            );
        }
    }
    if slugs.is_empty() {
        // Nothing to fetch — and nothing to write: an empty slug set
        // must never stamp over a good cache.
        return (HashMap::new(), json!({"state": "ok", "as_of": now}));
    }
    let claimed = {
        let mut running = GH_REFRESHING.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains(&file) {
            false
        } else {
            running.push(file.clone());
            true
        }
    };
    if claimed {
        let (tx, rx) = mpsc::channel();
        let (dir, want, prior) = (state_dir.to_path_buf(), slugs.to_vec(), cached.clone());
        std::thread::spawn(move || {
            let out = refresh_github(&dir, &want, prior, fetch);
            GH_REFRESHING
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|f| f != &file);
            let _ = tx.send(out);
        });
        if let Ok(out) = rx.recv_timeout(wait) {
            return out;
        }
    }
    stale_github(
        cached,
        slugs,
        Some(format!(
            "github refresh still running after {:.1}s — serving the last cache",
            wait.as_secs_f64()
        )),
    )
}

/// The last good rows for this slug set, untimed — better stale rows
/// than blank ones. `unavailable` when the cache covers none of them.
fn stale_github(
    cached: Option<GhCache>,
    slugs: &[String],
    error: Option<String>,
) -> (HashMap<String, Value>, Value) {
    let cached = cached.filter(|c| c.slugs.as_slice() == slugs);
    let at = cached.as_ref().map(|c| c.at);
    let stale: HashMap<String, Value> = cached
        .map(|c| {
            c.repos
                .into_iter()
                .filter(|(k, _)| slugs.contains(k))
                .collect()
        })
        .unwrap_or_default();
    if stale.is_empty() {
        return (
            stale,
            json!({"state": "unavailable", "error": error, "as_of": null}),
        );
    }
    (
        stale,
        json!({"state": "stale", "error": error, "as_of": at}),
    )
}

/// Fetch every slug concurrently (each `gh` call bounded by
/// [`super::GH_TIMEOUT`]), fill failed slugs from the cache, and write the
/// cache when anything came back.
fn refresh_github(
    state_dir: &Path,
    slugs: &[String],
    cached: Option<GhCache>,
    fetch: GhFetch,
) -> (HashMap<String, Value>, Value) {
    let results: Vec<Result<Value, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = slugs
            .iter()
            .map(|slug| s.spawn(move || fetch(slug)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err("gh fetch panicked".to_string()))
            })
            .collect()
    });
    let now = now_epoch();
    let mut repos = HashMap::new();
    let mut first_err = None;
    for (slug, result) in slugs.iter().zip(results) {
        match result {
            Ok(v) => {
                repos.insert(slug.clone(), v);
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if repos.is_empty() {
        // Every call failed: keep the last good rows for this slug set.
        return stale_github(cached, slugs, first_err);
    }
    // Partial failure: stale rows fill the missing slugs only when the
    // cache belongs to this slug set. A partial result is not a fresh
    // snapshot even if the failed slug had no cached row.
    let prior = cached.filter(|c| c.slugs.as_slice() == slugs);
    let prior_at = prior.as_ref().map(|c| c.at);
    if let Some(c) = prior {
        for slug in slugs {
            if !repos.contains_key(slug) {
                if let Some(v) = c.repos.get(slug) {
                    repos.insert(slug.clone(), v.clone());
                }
            }
        }
    }
    if first_err.is_some() {
        // Keep the oldest applicable snapshot age so the next request
        // retries. Without a matching cache, do not persist an
        // incomplete result as a fresh cache hit.
        if let Some(at) = prior_at {
            let _ = std::fs::create_dir_all(state_dir);
            write_cache(&cache_file(state_dir), slugs, &repos, at);
        }
        return (
            repos,
            json!({"state": "stale", "error": first_err, "as_of": prior_at}),
        );
    }
    let _ = std::fs::create_dir_all(state_dir);
    write_cache(&cache_file(state_dir), slugs, &repos, now);
    (repos, json!({"state": "ok", "as_of": now}))
}
