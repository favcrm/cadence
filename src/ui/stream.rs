//! `/api/stream` — server-sent events. One open connection per tab
//! carries the read model's invalidation frames: `stream_events`
//! subscribes and writes each frame straight onto the socket until the
//! client or the watcher drops it.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tiny_http::Request;

use super::read_model;
use crate::issue::{project, Pm};

/// Newest mtime among regular files under `dir` — the tracker-change
/// fingerprint. Small tree; a full walk every poll is still cheap.
pub(crate) fn dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let m = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if newest.is_none_or(|n| m > n) {
                    newest = Some(m);
                }
            }
        }
    }
    newest
}

/// A cheap content fingerprint for a JSON value — the serialized bytes
/// through a stable hasher (no new dependency for one hash).
pub(crate) fn value_fp(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(value).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// The exact collection representation, shared with SSE aggregate change
/// detection so config/count changes invalidate and title-only edits do not.
/// Counts come from the tracker's already-loaded issues — a title edit must
/// not re-parse every folder to learn that the counts did not move.
pub(crate) fn projects_payload(pm: &Pm, counts: &HashMap<String, usize>) -> Value {
    let projects = project::list(&pm.dir).unwrap_or_default();
    let payload: Vec<Value> = projects.iter().map(|p| json!({
        "key": p.key, "prefix": p.prefix, "components": p.components,
        "tags": p.tags, "default_owner": p.default_owner,
        "repos": p.repos.iter().map(|r| json!({"path": r.path, "remote": r.remote})).collect::<Vec<_>>(),
        "issues": counts.get(&p.key).copied().unwrap_or(0),
    })).collect();
    json!({"projects": payload})
}

/// The board resources one stream event invalidates, sent as the frame's
/// data (`{"resources":[...]}`) so the client refetches only those. The
/// event name stays the change source, which older clients key on.
/// - tracker files → the cards, the per-project list, the open issue
///   and the overview's tracker rows;
/// - jobs → card status (job outcomes) and agent task bindings;
/// - agents → agent rows and their `by_issue` strip (cards read their
///   agent chips from it), the open issue's agents, overview needs;
/// - monitors → the overview's monitoring block only.
pub(crate) fn event_resources(name: &str) -> &'static [&'static str] {
    match name {
        "issues" => &[
            "issues",
            "projects",
            "issue",
            "overview",
            "workflows",
            "apps",
            "app",
            "app_runs",
            "outbox",
            "app_outputs",
        ],
        "jobs" => &[
            "issues",
            "agents",
            "issue",
            "overview",
            "app_runs",
            "outbox",
            "app_outputs",
        ],
        // A released publish lands an outbox item — the same event the
        // effect row's state change produces. Agent rows change what an
        // app's workflow checks resolve to, so apps refetch too.
        "agents" => &["agents", "issue", "overview", "outbox", "apps", "app"],
        "monitoring" => &["overview"],
        _ => &[
            "issues",
            "projects",
            "agents",
            "issue",
            "overview",
            "workflows",
            "apps",
            "app",
            "app_runs",
            "outbox",
            "app_outputs",
        ],
    }
}

/// `GET /api/stream` — server-sent events written straight onto the
/// socket. tiny_http's chunked path buffers small writes inside
/// `chunked_transfer::Encoder` (it flushes only on `flush()` or a full
/// chunk), so a reader-based `Response` would never emit a small SSE
/// frame. `into_writer` hands over the socket: the head is written by
/// hand, each frame flushes immediately, and dropping the writer on
/// exit closes the stream — which is also how a dead client surfaces.
pub(crate) fn stream_hello(entities: bool) -> String {
    let mut data = json!({"build": crate::overview::BUILD_ID});
    if entities {
        data["entities"] = json!(true);
    }
    format!("event: hello\ndata: {data}\n\n")
}

pub(crate) fn stream_events(request: Request, state_dir: &Path, pm_dir: &Path) {
    let entities = request
        .url()
        .split_once('?')
        .is_some_and(|(_, q)| q.split('&').any(|part| part == "entities=1"));
    let mut w = request.into_writer();
    // Join the board's shared watcher before the head goes out: the
    // first subscriber's baseline is taken inside `subscribe`, so the
    // client's first action after it sees the stream live cannot be
    // absorbed into it (CAD-325: one watcher per board, not per client).
    let rx = read_model::get(state_dir, pm_dir).subscribe();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Cache-Control: no-store\r\nConnection: close\r\n\r\n";
    if w.write_all(head.as_bytes())
        .and_then(|_| w.flush())
        .is_err()
    {
        return;
    }
    let frame = |w: &mut dyn Write, bytes: &[u8]| -> bool {
        w.write_all(bytes).and_then(|_| w.flush()).is_ok()
    };
    // First frame immediately — `hello` names the serving build (a tab
    // running an older bundle compares and prompts a reload, CAD-573)
    // and the `: ping` right behind it proves the stream is live and
    // gives proxies something to flush before the first event exists.
    let hello = stream_hello(entities);
    if !frame(&mut w, hello.as_bytes()) || !frame(&mut w, b": ping\n\n") {
        return;
    }
    // `: ping` every 15 s of wire silence — the watcher's per-second
    // heartbeat would otherwise starve the keepalive, and an idle dead
    // client would never surface without a write.
    let mut ping_at = Instant::now() + Duration::from_secs(15);
    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(f) if &*f == read_model::HEARTBEAT => {}
            Ok(f) => {
                if !entities && f.starts_with("event: aggregates\n") {
                    continue;
                }
                let optimized = entities.then(|| read_model::entity_frame(&f));
                let bytes = optimized.as_deref().unwrap_or(&f);
                if !frame(&mut w, bytes.as_bytes()) {
                    return;
                }
                ping_at = Instant::now() + Duration::from_secs(15);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() >= ping_at {
            let ping: &[u8] = if entities {
                b"event: heartbeat\ndata: {}\n\n"
            } else {
                b": ping\n\n"
            };
            if !frame(&mut w, ping) {
                return;
            }
            ping_at = Instant::now() + Duration::from_secs(15);
        }
    }
}
