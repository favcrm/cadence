//! CAD-2215: private-work classification for portable coordination packages.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::super::quote_ident;
use super::{
    check_tracker_content, check_tracker_path, git_bytes, git_plain, sql_string, Member,
    PKG_TRACKER,
};
use crate::error::{Error, Result};

// ---------- CAD-2215: private-work classification ----------

/// CAD-2215: private or private-capable work the coordination profile cannot
/// carry. Every table here holds member identity or ownership, an app
/// conversation binding, an app grant, private chat/artifact content, a
/// human-room transcript or cloud-agent state. They are all outside the
/// profile, so a package built from a workspace that holds them would publish
/// the surrounding jobs, messages and tracker while silently dropping the
/// ownership, grants, receipts or bytes that make them private. ANY row
/// refuses the export, in any state: a revoked grant is still a tombstone
/// naming the member and conversation, a queued row is a request whose job
/// may not exist yet, and a single chat row is an upload whose bytes are not
/// in the package. The second element is the refusal vocabulary — a class
/// name, never a member, job, conversation, file or request.
const PRIVATE_STORE_TABLES: &[(&str, &str)] = &[
    // Queued-but-unpromoted: accepted, maybe half-bound, no ledger row.
    ("assist_request_queue", "queued member requests"),
    // The owning member's request ledger, the share list, the stall
    // provenance and the job subscription all name the person.
    ("assist_requests", "member-private job ownership"),
    ("assist_job_shares", "member-private job ownership"),
    ("assist_stall_checks", "member-private job ownership"),
    ("job_subscribers", "member job subscriptions"),
    // Per-member assistant bindings carry the member's private conversation.
    ("assistants", "member assistants"),
    // Active grants AND revoked tombstones: both are exact per-member
    // conversation bindings.
    ("app_member_grants", "app member grants"),
    // App contexts (per-request app state) and the workspace's app team
    // default: the coordination profile carries neither, and either can
    // name a member's work.
    ("app_contexts", "app contexts"),
    // Chat attachments and issue artifacts: content plus the member identity
    // that uploaded or produced it. Both byte stores live outside the
    // package, so publishing the surrounding rows would drop the work.
    //
    // `chat_upload_accounts` is NOT proof of an upload: an account row is
    // minted on operator login as well as at upload. It is refused
    // conservatively because an account is the precondition for any
    // operational put, and the operational custody root cannot be inspected
    // from an export; refusing every workspace that ever minted one keeps that
    // unreadable root empty for every export that proceeds. The cost is real:
    // a board where anyone logged in through the upload-session RPC refuses a
    // coordination export even if it never uploaded.
    ("chat_files", "chat files"),
    (
        "chat_upload_accounts",
        "chat upload accounts (conservative)",
    ),
    ("artifacts", "artifacts"),
    // Human-room transcripts and membership. The projection used to strip
    // these rows; a source holding them now refuses instead, so no private
    // human conversation is silently dropped.
    ("human_rooms", "human-room transcripts"),
    ("human_room_events", "human-room transcripts"),
    ("human_room_members", "human-room transcripts"),
    ("human_room_mutes", "human-room transcripts"),
    ("human_room_post_meta", "human-room transcripts"),
    ("human_room_post_requests", "human-room transcripts"),
    ("human_room_state", "human-room transcripts"),
    // Cloud-agent connections, grants, sessions and operations, and the
    // request/result text they own. Projected away before; now a refused
    // source, never a publication without its authority rows.
    ("cloud_agents", "cloud-agent state"),
    ("cloud_agent_sources", "cloud-agent state"),
    ("cloud_agent_sessions", "cloud-agent state"),
    ("cloud_agent_operations", "cloud-agent state"),
];

/// CAD-2215: prove the source holds no private work before a coordination
/// package exists.
///
/// This is a refusal, not a scrub. The coordination profile exports jobs,
/// messages, threads, events and the tracker, but nothing that makes member
/// work private — so a workspace with private work cannot be exported
/// safely at any fidelity. Classification is deliberately presence-based
/// and fail-closed: any row (active, revoked, queued, tombstones included),
/// any per-installation app record file, chat attachment, artifact, human-room
/// topic or cloud-agent marker, any recorded private delivery, any private
/// project policy or any publication marker refuses, and so does a record that
/// cannot be read or decoded. Nothing is published: the pre-check runs before
/// staging exists, a later refusal removes the private staging dir, and the
/// source store, tracker and state dir are only ever read (see
/// `export_portable`).
///
/// `db` is the database to classify — the live store for the pre/post checks
/// and the private snapshot for the authoritative one. `tracker` is the
/// tracker whose committed tree `head` would be copied (the private capture
/// for a snapshot export). The export calls this three times: before any
/// snapshot copy is written (a refusal creates nothing), on the snapshot that
/// the package is projected from, and on the live source once the package is
/// complete, because the store, tracker and private ledger are not one atomic
/// snapshot. Only counts and fixed class names leave this function; no column
/// value, path content or member identity does.
pub(super) fn classify_private_work(
    db: &Path,
    state_dir: &Path,
    workspace: &Path,
    tracker: &Path,
    head: &str,
    attest_chat_custody_absent: bool,
) -> Result<()> {
    // The proven cross-process read of the daemon store: read-only, shared
    // busy timeout, never creates or migrates the file. The live store may
    // be inside a daemon write, so a transient lock waits rather than reads
    // as an unclassifiable workspace.
    let conn = crate::store::open_read_only(db)?;
    let mut found: Vec<&'static str> = Vec::new();
    for &(table, class) in PRIVATE_STORE_TABLES {
        if count_where(&conn, table, "1")? > 0 {
            found.push(class);
        }
    }
    if app_conversation_rows(&conn)? > 0 {
        found.push("app conversations");
    }
    // Human-room transcripts reuse the coordination topic tables; the
    // projection used to delete them, which published the room's remaining
    // metadata without its posts. Presence now refuses instead.
    if count_where(&conn, "topics", "type='human_room'")? > 0 {
        found.push("human-room transcripts");
    }
    // Cloud traffic is also marked on the coordination tables: the alias
    // carries the provider and the messages carry the source. Refusing both
    // markers means the projection's cloud strip is never the only thing
    // standing between a cloud conversation and a package.
    let cloud_provider = format!("provider={}", sql_string(crate::cloud_agent::PROVIDER));
    if count_where(&conn, "agents", &cloud_provider)? > 0
        || count_where(&conn, "messages", "source='cloud_agent'")? > 0
    {
        found.push("cloud-agent state");
    }
    if count_where(
        &conn,
        "connection_policies",
        "visibility='personal' OR owner_user_id IS NOT NULL",
    )? > 0
    {
        found.push("personal connection policies");
    }
    drop(conn);
    classify_state_dir(state_dir, &mut found)?;
    // Chat attachment custody has no single establishable root.
    //
    // The legacy PM-rooted custody is `<pm dir>/.cadence/chat-files`, and a
    // nested tracker (`pm/pm.yaml`) roots its PM at `<tracker>/pm`. Both
    // admitted layouts are inspected here, so no PM-dir guess is needed.
    refuse_state_entries(
        &workspace.join(".cadence"),
        "chat-files",
        "chat files",
        &mut found,
    )?;
    refuse_state_entries(
        &workspace.join("pm").join(".cadence"),
        "chat-files",
        "chat files",
        &mut found,
    )?;
    classify_tracker(tracker, head, &mut found)?;
    if found.is_empty() && !attest_chat_custody_absent {
        // The operational custody root — per-user trees under the workspace
        // the daemon resolved from its CAPTURED provider environment — has no
        // durable binding: it is in-process only, and neither the store nor
        // the state directory records it. `restore --force` can replace the
        // store with an older copy while that root keeps attachment bytes no
        // surviving row names, so neither an empty `chat_files` nor an empty
        // `chat_upload_accounts` proves it empty. The root is never guessed,
        // and an export that cannot prove it empty refuses unless the operator
        // states the fact for this source (`--attest-chat-custody-absent`).
        return Err(Error::rejected(
            "portable export refused: chat attachment custody cannot be proven absent for this \
             source. The daemon resolves an operational upload workspace from its in-process \
             provider environment and persists no binding; a store restore can replace the \
             database while an earlier root keeps attachment bytes that no surviving row names. \
             Proving absence needs a complete, restore-surviving record of every operational \
             workspace this store has ever used, and no such record exists; a forward-only \
             sidecar or the current environment cannot establish history. Re-run with \
             `--attest-chat-custody-absent` to state for this source that no such root holds \
             attachment bytes. No package was written and the source is unchanged.",
        ));
    }
    if found.is_empty() {
        return Ok(());
    }
    let mut classes: Vec<&'static str> = Vec::with_capacity(found.len());
    for class in found {
        if !classes.contains(&class) {
            classes.push(class);
        }
    }
    Err(Error::rejected(format!(
        "portable export refused: this workspace holds private work ({}). A coordination \
         package carries jobs, messages, threads, events and the tracker, but not the member \
         ownership, grants, conversations and receipts that make that work private, so \
         publishing one would expose private work without its provenance. No package was \
         written and the source is unchanged. Keep the source workspace (its store, tracker \
         and state directory) as the record of that private work, and export a workspace \
         that holds none of it.",
        classes.join(", ")
    )))
}

/// State-directory custody roots beside the store that hold private or
/// private-capable content and are never package members, so no package can
/// disclose them and no export may drop them silently. Any entry refuses; an
/// absent root, or an empty real directory (it holds nothing), stays clean.
const PRIVATE_STATE_DIRS: &[(&str, &str)] = &[
    // `store::app_records::RECORDS_DIR`: one app-owned SQLite file per
    // installation. A grant and a conversation thread can both be gone while
    // the record file remains, so no store marker can prove it holds no
    // member work. Every entry counts, including WAL sidecars and
    // interrupted-restore dotfiles: calling one class "ordinary" would be a
    // guess.
    (crate::store::app_records::RECORDS_DIR, "app record state"),
    // `store::chat_files::CHAT_FILES_DIR` and `store::artifacts::ARTIFACTS_DIR`
    // (private module constants; these are their on-disk names): chat
    // attachments and issue artifact bytes, plus the member identity that
    // uploaded or produced them.
    ("chat-files", "chat files"),
    ("artifacts", "artifacts"),
    // `daemon::members_rpc`'s private job spec custody. A crash can leave a
    // spec whose queue/ledger binding never landed, so any entry — and any
    // unreadable or non-directory path — blocks instead of being read as "no
    // private job".
    ("assist_jobs", "member-private job specs"),
    // `wiki::UPLOAD_DIR`: the one staging root uploads are read through.
    // A successful put moves the file out, so an entry here is an upload this
    // export cannot attribute — chat attachment or workspace wiki file — and
    // is refused rather than guessed as ordinary.
    (crate::wiki::UPLOAD_DIR, "wiki upload staging"),
];

/// `daemon::app_teams_rpc`'s workspace app-team default file.
const APP_TEAMS_FILE: &str = "app-teams.json";

/// State-directory artifacts beside the store that can hold private member
/// work and are never package members, so no package can disclose them and
/// no export can drop them silently.
///
/// * Every root in [`PRIVATE_STATE_DIRS`] refuses on any entry.
/// * `app-teams.json` names each installation's owner PM and workflow roles.
///   A coordination package does not carry it and cannot prove an
///   installation's team is unrelated to member work, so any recorded team
///   refuses; an absent file or an empty JSON object is clean, and an
///   unreadable, linked or non-JSON file refuses rather than reading as "no
///   team".
/// * `delivery-private.json` is the durable private delivery and publication
///   ledger: results, receipts and publication attempts for member-private
///   jobs. Any recorded delivery refuses, and the loader treats an unreadable
///   ledger as an error, never as an empty one.
///
/// This is not a claim to identify arbitrary private text with no
/// provenance: an artifact that leaves no row, no ledger record and no marker
/// here is outside what this classification can observe, and the docs say so.
fn classify_state_dir(state_dir: &Path, found: &mut Vec<&'static str>) -> Result<()> {
    for &(name, class) in PRIVATE_STATE_DIRS {
        refuse_state_entries(state_dir, name, class, found)?;
    }
    classify_app_teams(state_dir, found)?;
    let deliveries = crate::delivery::private::load(state_dir)
        .map_err(|error| refuse_unclassifiable_state(&error.to_string()))?;
    if !deliveries.is_empty() {
        found.push("private delivery/publication state");
    }
    Ok(())
}

/// Refuse when `<state_dir>/<name>` holds any entry. A missing path, or an
/// empty real directory, holds nothing.
fn refuse_state_entries(
    state_dir: &Path,
    name: &str,
    class: &'static str,
    found: &mut Vec<&'static str>,
) -> Result<()> {
    let path = state_dir.join(name);
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(refuse_unclassifiable_state(&format!(
            "the {name} root beside the store cannot be read ({error})"
        ))),
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            Err(refuse_unclassifiable_state(&format!(
                "the {name} path beside the store is not a real directory"
            )))
        }
        Ok(_) => {
            let mut entries = fs::read_dir(&path).map_err(|error| {
                refuse_unclassifiable_state(&format!(
                    "the {name} root beside the store cannot be listed ({error})"
                ))
            })?;
            if entries.next().is_some() {
                found.push(class);
            }
            Ok(())
        }
    }
}

/// Refuse when the workspace app-team default file records any team.
fn classify_app_teams(state_dir: &Path, found: &mut Vec<&'static str>) -> Result<()> {
    let path = state_dir.join(APP_TEAMS_FILE);
    let meta = match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(refuse_unclassifiable_state(&format!(
                "the app team record beside the store cannot be read ({error})"
            )))
        }
        Ok(meta) => meta,
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.nlink() != 1 {
        return Err(refuse_unclassifiable_state(
            "the app team record beside the store is not a plain single-link regular file",
        ));
    }
    let bytes = fs::read(&path).map_err(|error| {
        refuse_unclassifiable_state(&format!(
            "the app team record beside the store cannot be read ({error})"
        ))
    })?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
        refuse_unclassifiable_state("the app team record beside the store is not valid JSON")
    })?;
    match value.as_object() {
        Some(teams) if teams.is_empty() => Ok(()),
        Some(_) => {
            found.push("app teams");
            Ok(())
        }
        None => Err(refuse_unclassifiable_state(
            "the app team record beside the store is not a JSON object",
        )),
    }
}

/// The refusal for a state-directory artifact this export cannot read well
/// enough to prove it holds no private member work. The reason names the
/// artifact and never its content.
fn refuse_unclassifiable_state(why: &str) -> Error {
    Error::rejected(format!(
        "portable export refused: {why}, so private member app data cannot be ruled out; \
         no package was written and the source is unchanged"
    ))
}

/// `count(*)` over `table` (quoted) with an internal predicate, or 0 when
/// the store predates the table. The caller has already proved the table's
/// exact columns, so this only counts rows; no column value is read.
pub(super) fn count_where(conn: &Connection, table: &str, predicate: &str) -> Result<i64> {
    let present: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [table],
        |r| r.get(0),
    )?;
    if present == 0 {
        return Ok(0);
    }
    Ok(conn.query_row(
        &format!(
            "SELECT count(*) FROM {} WHERE {predicate}",
            quote_ident(table)
        ),
        [],
        |r| r.get(0),
    )?)
}

/// App installation conversations are `threads` rows with an `install_id`
/// (CAD-1098). A conversation is private whether or not a grant names it.
/// A store whose `threads` predates that column has none.
pub(super) fn app_conversation_rows(conn: &Connection) -> Result<i64> {
    if !has_column(conn, "threads", "install_id")? {
        return Ok(0);
    }
    count_where(conn, "threads", "install_id IS NOT NULL")
}

/// Is `column` one of `table`'s physical columns? Used only to prove an
/// internal predicate can run against a store that may predate the column;
/// absence is "no such rows", never an inferred value.
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let present: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM pragma_table_info({}) WHERE name=?1",
            sql_string(table)
        ),
        [column],
        |r| r.get(0),
    )?;
    Ok(present > 0)
}

/// Private tracker content, read from the SAME committed tree `copy_tracker`
/// packages, so a snapshot export classifies exactly what it would copy.
///
/// * A project whose recorded policy is `visibility: private` hides its
///   tickets behind grants; packaging the tracker would publish them while
///   leaving the policy that restricted them behind.
/// * A publication comment is the tracker-side receipt that a
///   member-private delivery was shared here. The other half (which
///   delivery, which results, which attempt) is the private ledger beside
///   the store, which no package carries.
///
/// A project record that cannot be decoded refuses: an unreadable policy is
/// not a public one. The tracker admission already refused symlinks and
/// non-regular files, so every listed path is a real blob.
fn classify_tracker(tracker: &Path, head: &str, found: &mut Vec<&'static str>) -> Result<()> {
    let listing = git_plain(tracker, &["ls-tree", "-r", "-z", "--full-tree", head])?;
    let mut paths: Vec<&str> = Vec::new();
    for record in listing.split('\0').filter(|r| !r.is_empty()) {
        let (_, path) = record
            .split_once('\t')
            .ok_or_else(|| Error::rejected("malformed tracker listing"))?;
        paths.push(path);
    }
    // The PM root comes from the SAME tree `copy_tracker` reads. Asking the
    // worktree could say "no pm.yaml" while `head` still carries one, which
    // would skip every project record the package would then include.
    let pm_root = pm_root_from_paths(paths.iter().copied())?;
    classify_tracker_paths(
        paths.iter().copied(),
        pm_root.as_deref(),
        "export",
        |path| git_bytes(tracker, &["show", &format!("{head}:{path}")]),
        found,
    )
}

/// Classify a tracker member set. `read` returns the exact bytes of one
/// tracker-relative path, so the caller decides whether they come from a Git
/// blob or a copied/staged file — classification never re-derives paths and
/// never trusts a tree layout the caller did not enumerate.
fn classify_tracker_paths<'a, F>(
    paths: impl Iterator<Item = &'a str>,
    pm_root: Option<&str>,
    what: &str,
    mut read: F,
    found: &mut Vec<&'static str>,
) -> Result<()>
where
    F: FnMut(&str) -> Result<Vec<u8>>,
{
    let paths: Vec<&str> = paths.collect();
    refuse_ambiguous_project_layout(&paths, pm_root, what)?;
    for path in paths {
        // Publication evidence needs no PM root: the file name is proof on
        // its own, so a tracker without `pm.yaml` is still scanned for it.
        if publication_comment(path) {
            found.push("private delivery/publication state");
            continue;
        }
        let Some(root) = pm_root else {
            continue;
        };
        if project_record_key(root, path).is_none() {
            continue;
        }
        let bytes = read(path)?;
        if project_record_is_private(path, &bytes, what)? {
            found.push("private project policy");
        }
    }
    Ok(())
}

/// Refuse project-shaped records that no carried PM root can authorize, and a
/// second project layout beside the authoritative one. Both are ambiguous
/// evidence of a private policy, never permission to ignore it.
fn refuse_ambiguous_project_layout(
    paths: &[&str],
    pm_root: Option<&str>,
    what: &str,
) -> Result<()> {
    let mut at_root = false;
    let mut nested = false;
    for path in paths {
        if project_record_key(".", path).is_some() {
            at_root = true;
        }
        if project_record_key("pm", path).is_some() {
            nested = true;
        }
    }
    let unproven = match pm_root {
        None => at_root || nested,
        Some(".") => nested,
        Some("pm") => at_root,
        Some(_) => true,
    };
    if unproven {
        return Err(Error::rejected(format!(
            "portable {what} refused: the tracker carries project-shaped records whose PM root \
             is missing or ambiguous, so a private project policy cannot be ruled out; nothing \
             was published"
        )));
    }
    Ok(())
}

/// Decode one project record and answer whether it records a private policy.
/// Shared by the Git classifier, the copied-tree classifier and the per-member
/// package check, so all three apply the same rule to the same bytes.
fn project_record_is_private(path: &str, bytes: &[u8], what: &str) -> Result<bool> {
    let decoded =
        serde_yaml::from_slice::<crate::issue::project::Project>(bytes).map_err(|_| {
            Error::rejected(format!(
                "portable {what} refused: tracker project record {path} cannot be decoded, so a \
             private project policy cannot be ruled out; nothing was published"
            ))
        })?;
    Ok(decoded
        .policy
        .as_ref()
        .is_some_and(|policy| policy.visibility == crate::issue::project::Visibility::Private))
}

/// `<key>/project.yaml` or `pm/<key>/project.yaml`: project-shaped in either
/// admitted layout, whatever PM root the tree carries.
fn project_shaped(path: &str) -> bool {
    project_record_key(".", path).is_some() || project_record_key("pm", path).is_some()
}

/// A project record sits at exactly `<pm-root>/<key>/project.yaml`; nothing
/// deeper or shallower is one, and the PM root already came from `pm.yaml`.
fn project_record_key<'a>(pm_root: &str, path: &'a str) -> Option<&'a str> {
    let rel = if pm_root == "." {
        path
    } else {
        path.strip_prefix("pm/")?
    };
    let (key, file) = rel.split_once('/')?;
    if key.is_empty() || file != "project.yaml" {
        return None;
    }
    Some(key)
}

/// The publication writer keys its comment `publication`, so a tracker file
/// ending `-publication.md` under a ticket's `comments/` dir is one of its
/// publications. The name is not caller-chosen: `add_comment_keyed` is
/// internal, so this is evidence, not a guess.
fn publication_comment(path: &str) -> bool {
    path.split('/').any(|part| part == "comments")
        && path
            .rsplit('/')
            .next()
            .is_some_and(|name| name.ends_with("-publication.md"))
}

/// The PM root carried by an enumerated tracker member set. Mirrors
/// `pm_root_of`'s ambiguity refusal, but over the exact paths being packaged
/// or verified, so classification, copy and manifest verification can never
/// disagree about which project records exist.
pub(super) fn pm_root_from_paths<'a>(
    paths: impl Iterator<Item = &'a str>,
) -> Result<Option<String>> {
    let mut at_root = false;
    let mut nested = false;
    for path in paths {
        match path {
            "pm.yaml" => at_root = true,
            "pm/pm.yaml" => nested = true,
            _ => {}
        }
    }
    match (at_root, nested) {
        (true, true) => Err(Error::rejected(
            "tracker has pm.yaml at its root and in pm/; the PM root is ambiguous",
        )),
        (true, false) => Ok(Some(".".into())),
        (false, true) => Ok(Some("pm".into())),
        (false, false) => Ok(None),
    }
}

/// Tracker-relative regular files under a copied tracker tree, sorted.
/// Symlinks and special files refuse rather than being skipped.
pub(super) fn walk_tracker_paths(dir: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)? {
            let path = entry?.path();
            let meta = path.symlink_metadata()?;
            if meta.file_type().is_symlink() {
                return Err(Error::rejected("tracker copy contains a symlink"));
            }
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                out.push(
                    path.strip_prefix(dir)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned(),
                );
            } else {
                return Err(Error::rejected("tracker copy contains a non-regular file"));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Walk a copied package tracker once, and for each file read the bytes ONCE,
/// then hash and classify that same buffer. The returned members are exactly
/// what the manifest will record; a private or undecodable record refuses, as
/// does project-shaped evidence the copied tree's PM root cannot authorize.
pub(super) fn package_tracker_members(
    tracker: &Path,
    pm_root: Option<&str>,
) -> Result<Vec<Member>> {
    let paths = walk_tracker_paths(tracker)?;
    let as_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    refuse_ambiguous_project_layout(&as_refs, pm_root, "export")?;
    let mut members = Vec::with_capacity(paths.len());
    for rel in paths {
        let file = tracker.join(&rel);
        let bytes = fs::read(&file).map_err(|error| {
            Error::rejected(format!(
                "portable export refused: tracker file {rel} cannot be read ({error}); nothing \
                 was published"
            ))
        })?;
        check_tracker_path(&rel)?;
        check_tracker_content(&rel, &bytes)?;
        check_tracker_private_content("export", pm_root, &rel, &bytes)?;
        members.push(Member {
            path: format!("{PKG_TRACKER}/{rel}"),
            bytes: bytes.len() as u64,
            sha256: hex_digest(&bytes),
        });
    }
    Ok(members)
}

/// Lowercase hex SHA-256 of the exact bytes a check classified.
pub(super) fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// CAD-2215: a package's tracker copy must not carry content whose provenance
/// the coordination profile cannot carry. Runs on the exact bytes
/// `verify_manifest_bytes` already hashed and matched, so `inspect-portable`
/// and every restore refuse an older or crafted package instead of admitting
/// it:
///
/// * a publication comment is the tracker-side receipt of a member-private
///   delivery, and no package carries the delivery it names;
/// * a project record whose policy is `visibility: private` hides its tickets
///   behind grants that are not in the package, so the tickets would be
///   admitted to every reader. A project-shaped record the carried PM root
///   cannot authorize, or one this binary cannot decode, refuses too: an
///   unreadable policy is not a public one.
pub(super) fn check_tracker_private_content(
    what: &str,
    pm_root: Option<&str>,
    path: &str,
    bytes: &[u8],
) -> Result<()> {
    if publication_comment(path) {
        return Err(Error::rejected(format!(
            "portable {what} refused: tracker file {path} is a publication comment; a \
             coordination package cannot carry the receipt of a member-private delivery"
        )));
    }
    let authorized = pm_root.is_some_and(|root| project_record_key(root, path).is_some());
    if !authorized {
        if !project_shaped(path) {
            return Ok(());
        }
        return Err(Error::rejected(format!(
            "portable {what} refused: tracker file {path} is project-shaped but the carried \
             PM root does not authorize it, so a private project policy cannot be ruled out"
        )));
    }
    if project_record_is_private(path, bytes, what)? {
        return Err(Error::rejected(format!(
            "portable {what} refused: tracker project record {path} records a private project \
             policy; a coordination package cannot carry it without its grants"
        )));
    }
    Ok(())
}
