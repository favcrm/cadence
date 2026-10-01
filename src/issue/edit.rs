//! `issue edit` — several single-purpose writes (set, tag, link, ref,
//! attach, acceptance, comment) as ONE transaction and ONE tracker
//! commit (CAD-887). Every part is parsed and validated before the
//! first file is written; a bad part refuses the whole edit with a
//! `rejected` error that names it, and nothing is written. The parts
//! run the same checks as their verbs — the helpers in [`write`] are
//! shared, not copied — under the same PM lock, with the same actor
//! and trailer derivation (`write::commit_who`, `write::comment_author`).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model;
use crate::issue::write::{self, check_tags};
use crate::issue::{parse, time, Pm};

/// What one `issue edit` asks for. Bodies arrive as text: the CLI
/// reads files and stdin, so the writer never touches either.
#[derive(Default)]
pub struct EditSpec {
    /// `key=value`, as `issue set`.
    pub set: Vec<String>,
    /// `+tag` adds, `-tag` removes, a bare `tag` adds.
    pub tags: Vec<String>,
    /// `kind:ID`, as `issue link`.
    pub link: Vec<String>,
    /// `kind:ID`, as `issue unlink`.
    pub unlink: Vec<String>,
    /// `kind:target`, or a bare http(s) URL (kind `url`).
    pub refs: Vec<String>,
    pub attach: Vec<PathBuf>,
    /// The acceptance checklist text, as `issue acceptance --from`.
    pub acceptance: Option<String>,
    pub comment: Option<String>,
    /// Comment author; only meaningful with a comment.
    pub author: Option<String>,
    /// Reason overriding the status=done evidence gate, as `set --force`.
    pub force: Option<String>,
}

/// `kind:value` → its two halves, naming `flag` when malformed.
fn split_kind<'a>(flag: &str, raw: &'a str) -> Result<(&'a str, &'a str)> {
    raw.split_once(':')
        .filter(|(k, v)| !k.is_empty() && !v.is_empty())
        .ok_or_else(|| Error::rejected(format!("edit {flag} '{raw}' is not kind:value")))
}

/// Run `f`, naming `flag` on any rejection so the caller sees which
/// part of the edit was bad.
fn part<T>(flag: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    f().map_err(|e| match e {
        Error::Rejected(m) => Error::rejected(format!("edit {flag}: {m}")),
        Error::Structured(mut st) => {
            st.message = format!("edit {flag}: {}", st.message);
            Error::Structured(st)
        }
        other => other,
    })
}

fn is_http(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

pub fn edit(
    pm: &Pm,
    id: &str,
    spec: &EditSpec,
    actor: &str,
    state_dir: Option<&Path>,
) -> Result<Value> {
    if spec.author.is_some() && spec.comment.is_none() {
        return Err(Error::rejected(
            "edit --author names a comment's author — pass --comment-file with it",
        ));
    }
    let (project, dir) = write::issue_dir(pm, id)?;

    // Phase 1 — parts that need no tracker state.
    let mut tag_delta: Vec<(bool, String)> = Vec::new();
    for raw in &spec.tags {
        let (add, tag) = match raw.strip_prefix('-') {
            Some(t) => (false, t),
            None => (true, raw.strip_prefix('+').unwrap_or(raw)),
        };
        part("--tag", || model::normalize_tags(&[tag.to_string()]))?;
        tag_delta.push((add, tag.to_string()));
    }
    let links: Vec<(bool, &str, &str)> = spec
        .link
        .iter()
        .map(|r| (false, r))
        .chain(spec.unlink.iter().map(|r| (true, r)))
        .map(|(unlink, raw)| {
            let flag = if unlink { "--unlink" } else { "--link" };
            part(flag, || {
                let (kind, target) = split_kind(flag, raw)?;
                model::check_link_kind(kind)?;
                model::check_id(target)?;
                Ok((unlink, kind, target))
            })
        })
        .collect::<Result<_>>()?;
    let refs: Vec<(String, String)> = spec
        .refs
        .iter()
        .map(|raw| {
            part("--ref", || {
                let (kind, target) = if is_http(raw) {
                    ("url", raw.as_str())
                } else {
                    split_kind("--ref", raw)?
                };
                model::check_ref_kind(kind)?;
                model::check_ref_value(target)?;
                Ok((kind.to_string(), target.to_string()))
            })
        })
        .collect::<Result<_>>()?;
    let mut attachments: Vec<(String, Vec<u8>)> = Vec::new();
    for file in &spec.attach {
        part("--attach", || {
            let meta = std::fs::metadata(file)
                .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))?;
            if !meta.is_file() {
                return Err(Error::rejected(format!("{} is not a file", file.display())));
            }
            if meta.len() > pm.config.artifact_max_bytes {
                return Err(Error::rejected(format!(
                    "{} is {} bytes — over the {} cap; link it with --ref instead",
                    file.display(),
                    meta.len(),
                    pm.config.artifact_max_bytes
                )));
            }
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .ok_or_else(|| Error::rejected("attachment needs a plain file name"))?;
            if !model::valid_artifact_name(&name) {
                return Err(Error::rejected(format!(
                    "bad artifact name '{name}' — [A-Za-z0-9._-]{{1,120}}, no leading dot"
                )));
            }
            attachments.push((name, std::fs::read(file)?));
            Ok(())
        })?;
    }
    let acceptance = match &spec.acceptance {
        Some(text) => Some(part("--acceptance", || {
            parse::parse_acceptance_input(text)
        })?),
        None => None,
    };
    let comment = match &spec.comment {
        Some(body) => Some(part("--comment-file", || {
            let author = write::comment_author(spec.author.as_deref())?;
            if body.trim().is_empty() {
                return Err(Error::rejected("comment body is empty"));
            }
            // CAD-109: a credential-shaped body is refused up front.
            let warnings = crate::secret::guard(&format!("{id}: comment"), body)?;
            Ok((author, warnings))
        })?),
        None => None,
    };

    // Phase 2 — under the lock, apply every part to the front and body
    // in memory. Nothing is on disk until all of it holds.
    let _lock = pm.lock()?;
    let (prev, prev_body) = write::load_front(&dir)?;
    let mut front = prev.clone();
    let mut body = prev_body.clone();
    let mut changed: Vec<String> = Vec::new();

    for (kind, target) in &refs {
        front
            .refs
            .push(write::new_ref(kind, target, None, None, None));
        changed.push(format!("ref:{kind}"));
    }
    for (unlink, kind, target) in &links {
        let flag = if *unlink { "--unlink" } else { "--link" };
        part(flag, || {
            write::apply_link(pm, &mut front, kind, target, *unlink)
        })?;
        changed.push(format!(
            "{}:{kind}:{target}",
            if *unlink { "unlink" } else { "link" }
        ));
    }
    if !spec.set.is_empty() {
        let keys = part("--set", || {
            write::apply_pairs(&project, &mut front, &spec.set)
        })?;
        changed.extend(keys.into_iter().map(|k| format!("set:{k}")));
    }
    if !tag_delta.is_empty() {
        let mut tags = front.tags.clone();
        for (add, tag) in &tag_delta {
            if *add {
                tags.push(tag.clone());
            } else {
                tags.retain(|t| t != tag);
            }
        }
        part("--tag", || {
            front.tags = check_tags(&project, &tags)?;
            Ok(())
        })?;
        if front.tags != prev.tags {
            changed.push(format!("tags={}", front.tags.join(",")));
        }
    }
    if let Some(items) = &acceptance {
        part("--acceptance", || {
            body = parse::replace_acceptance(&body, items)?;
            Ok(())
        })?;
        changed.push("acceptance".to_string());
    }
    // The gates `issue set` runs, over the final front: a `--ref pr:`
    // in the same edit counts as evidence for `status=done`.
    let forced = part("--set", || {
        write::check_set_gates(pm, &project, &prev, &front, spec.force.as_deref())
    })?;
    if !links.is_empty() {
        part("--link", || {
            let mut preview = crate::issue::board::load_all(&pm.dir, None)?;
            if let Some(this) = preview.iter_mut().find(|i| i.front.id == id) {
                this.front = front.clone();
            }
            write::check_structure(&preview, id)
        })?;
    }
    let issue_text = parse::render(&front, &body)?;
    let prev_text = parse::render(&prev, &prev_body)?;
    let file = dir.join("issue.md");
    let write_issue = issue_text != prev_text;
    changed.extend(attachments.iter().map(|(n, _)| format!("attach:{n}")));
    if comment.is_some() {
        changed.push("comment".to_string());
    }
    if !write_issue && attachments.is_empty() && comment.is_none() {
        return Err(Error::rejected(
            "edit changes nothing — pass at least one effective part \
             (--set, --tag, --link, --unlink, --ref, --attach, --acceptance, --comment-file)",
        ));
    }
    for sub in ["artifacts", "comments"] {
        let wanted = if sub == "artifacts" {
            !attachments.is_empty()
        } else {
            comment.is_some()
        };
        if wanted
            && dir
                .join(sub)
                .symlink_metadata()
                .is_ok_and(|m| m.is_symlink())
        {
            return Err(Error::rejected(format!(
                "{id}: {sub}/ is a symlink — refusing to write outside the PM dir"
            )));
        }
    }

    // Phase 3 — write everything, then one commit. Any failure undoes
    // every file this edit touched.
    let issue_prev = write::file_preimage(&file);
    let mut created: Vec<PathBuf> = Vec::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut comment_name = None;
    let written = (|| -> Result<Vec<String>> {
        if write_issue {
            write::save_front(&dir, &front, &body)?;
            paths.push(file.clone());
        }
        if !attachments.is_empty() {
            let artifacts = dir.join("artifacts");
            std::fs::create_dir_all(&artifacts)?;
            for (name, bytes) in &attachments {
                let path = write::create_exclusive(&artifacts, name, bytes)?;
                created.push(path.clone());
                paths.push(path);
            }
        }
        if let Some((author, _)) = &comment {
            let comments = dir.join("comments");
            std::fs::create_dir_all(&comments)?;
            let epoch = time::now_epoch();
            let meta = model::CommentFront {
                author: author.clone(),
                at: time::iso(epoch),
                kind: None,
            };
            let text = parse::render(&meta, spec.comment.as_deref().unwrap_or_default())?;
            let path = write::create_exclusive(
                &comments,
                &format!("{}-{author}.md", time::basic(epoch)),
                text.as_bytes(),
            )?;
            comment_name = path.file_name().map(|n| n.to_string_lossy().to_string());
            created.push(path.clone());
            paths.push(path);
        }
        let mut ids: Vec<&str> = vec![id];
        for (_, _, target) in &links {
            if !ids.contains(target) {
                ids.push(target);
            }
        }
        let mut summary = format!("{id}: edit {}", changed.join(" "));
        if let Some(reason) = &forced {
            summary.push_str(&format!(" (forced: {reason})"));
        }
        write::commit_who(pm, &paths, &summary, &ids, actor, spec.author.as_deref())
    })();
    let foreign = match written {
        Ok(f) => f,
        Err(e) => {
            if write_issue {
                write::restore_preimage(&file, issue_prev);
            }
            for path in &created {
                let _ = std::fs::remove_file(path);
            }
            return Err(e);
        }
    };
    let mut out = json!({
        "id": id,
        "rev": write::issue_rev(&dir)?,
        "changed": changed,
        "committed": true,
    });
    // `issue link` parity: lint-style warnings, and the CAD-757 notice
    // to the holders of a held ticket for each new blocked_by edge.
    out["warnings"] = json!(write::blocked_warnings(pm, id, state_dir)?);
    let notices: Vec<Value> = links
        .iter()
        .filter(|(unlink, kind, _)| !unlink && *kind == "blocked_by")
        .map(|(_, _, target)| {
            let mut n = crate::issue::blocked::notify_new_blocker(&front, target, state_dir);
            n["target"] = json!(target);
            n
        })
        .collect();
    if !notices.is_empty() {
        out["blocker_notices"] = json!(notices);
    }
    // `issue set` parity: a done ticket whose worktree ref is still
    // open tells the CLI to print the `issue finish` hint.
    if front.status == "done"
        && prev.status != "done"
        && front
            .refs
            .iter()
            .any(|r| r.kind == "worktree" && r.closed != Some(true))
    {
        out["worktree_open"] = json!([id]);
    }
    if let Some(name) = comment_name {
        out["comment"] = json!(name);
    }
    if let Some((_, warnings)) = &comment {
        if !warnings.is_empty() {
            out["secret_warnings"] = crate::secret::warnings_json(warnings);
        }
    }
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}
