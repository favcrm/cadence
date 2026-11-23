//! Hosted text edits share the revision-guarded `issue_set` write. For a
//! checklist replacement, the client uses the native Markdown parser on the
//! caller's exact revision, then sends the resulting body under the same CAS.

use super::{eprint_line, read_stdin_remote, Error, RemoteTarget, Result};
use cadence_agent::issue::{cli::IssueAction, parse, report::BODY_MAX};
use cadence_agent::remote_cli::{call_verb, RemoteAnswer};
use serde_json::{json, Map, Value};
use std::{io::Read, path::Path, time::Instant};

pub(super) fn needs_human(action: &IssueAction) -> bool {
    match action {
        IssueAction::Edit { .. } | IssueAction::Acceptance { .. } => true,
        IssueAction::Set { args, .. } => args.iter().any(|pair| text_key(pair).is_some()),
        _ => false,
    }
}

fn text_key(pair: &str) -> Option<&str> {
    pair.split_once('=')
        .map(|(key, _)| key)
        .filter(|key| matches!(*key, "body" | "acceptance"))
}

fn read_file(path: &Path, flag: &str) -> Result<String> {
    if path.as_os_str() == "-" {
        return read_stdin_remote(flag);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(BODY_MAX as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| {
            Error::rejected(format!("{flag}: cannot read {}: {error}", path.display()))
        })?;
    check_size(bytes.len())?;
    String::from_utf8(bytes).map_err(|_| Error::rejected(format!("{flag}: body is not UTF-8")))
}

fn check_size(size: usize) -> Result<()> {
    if size > BODY_MAX {
        return Err(Error::rejected(
            "issue body exceeds the 32 KB cap — trim it; nothing was sent",
        ));
    }
    Ok(())
}

pub(super) fn edit_args(action: &IssueAction) -> Result<Option<Map<String, Value>>> {
    let (id, rev, set, file) = match action {
        IssueAction::Edit {
            id,
            set,
            tag,
            link,
            unlink,
            refs,
            unrefs,
            attach,
            body,
            acceptance,
            comment_file,
            author,
            force,
            if_rev,
        } => {
            if !tag.is_empty()
                || !link.is_empty()
                || !unlink.is_empty()
                || !refs.is_empty()
                || !unrefs.is_empty()
                || !attach.is_empty()
                || comment_file.is_some()
                || author.is_some()
                || force.is_some()
            {
                return Err(Error::rejected(
                    "`cadence issue edit` on a hosted org takes --body, --acceptance, \
                     --set and --if-rev only; nothing was sent",
                ));
            }
            let file = body
                .as_deref()
                .map(|path| ("body", path, "issue edit --body"))
                .or_else(|| {
                    acceptance
                        .as_deref()
                        .map(|path| ("acceptance", path, "issue edit --acceptance"))
                });
            (id, if_rev, set.as_slice(), file)
        }
        IssueAction::Acceptance { id, from, if_rev } => (
            id,
            if_rev,
            &[][..],
            Some(("acceptance", from.as_path(), "issue acceptance --from")),
        ),
        _ => return Ok(None),
    };
    let Some(rev) = rev.as_deref().map(str::trim).filter(|rev| !rev.is_empty()) else {
        return Err(Error::rejected(
            "hosted issue edits need --if-rev <rev> from `issue show <id>`; nothing was sent",
        ));
    };
    if set.iter().any(|pair| !pair.contains('=')) || (set.is_empty() && file.is_none()) {
        return Err(Error::usage(
            "use --body FILE, --acceptance FILE or --set key=value; nothing was sent",
        ));
    }
    let mut pairs: Vec<Value> = set.iter().map(|pair| json!(pair)).collect();
    if let Some((key, path, flag)) = file {
        pairs.push(Value::String(format!("{key}={}", read_file(path, flag)?)));
    }
    Ok(Some(Map::from_iter([
        ("ids".into(), json!([id])),
        ("set".into(), Value::Array(pairs)),
        ("if_rev".into(), json!(rev)),
    ])))
}

pub(super) fn resolve(
    target: &RemoteTarget,
    args: &mut Map<String, Value>,
    deadline: Instant,
    wake_timeout: u64,
) -> Result<bool> {
    let [id] = args["ids"].as_array().unwrap().as_slice() else {
        unreachable!("issue_set takes one issue")
    };
    let pairs = args["set"].as_array().unwrap();
    let mut text = pairs.iter().enumerate().filter_map(|(index, pair)| {
        let (key, value) = pair.as_str()?.split_once('=')?;
        matches!(key, "body" | "acceptance").then_some((index, key, value))
    });
    let Some((index, key, value)) = text.next() else {
        return Ok(false);
    };
    if text.next().is_some() {
        return Err(Error::usage(
            "send one body or acceptance replacement per issue edit; nothing was sent",
        ));
    }
    check_size(value.len())?;
    if key == "body" {
        return Ok(true);
    }
    let items = parse::parse_acceptance_input(value)?;
    let current = call_verb(
        target,
        "issue_show",
        Map::from_iter([("id".into(), id.clone())]),
        deadline,
        wake_timeout,
        eprint_line,
    )?;
    let RemoteAnswer::Ok(current) = current else {
        return Err(Error::conflict_coded(
            "revision_conflict",
            "issue changed while reading its body; nothing was written",
        ));
    };
    let expected = args["if_rev"].as_str().expect("issue_set requires if_rev");
    let body = replacement(&current, expected, &items)?;
    check_size(body.len())?;
    args.get_mut("set").unwrap().as_array_mut().unwrap()[index] =
        Value::String(format!("body={body}"));
    Ok(true)
}

fn replacement(current: &Value, expected: &str, items: &[parse::AcceptanceItem]) -> Result<String> {
    let Some(rev) = current.get("rev").and_then(Value::as_str) else {
        return Err(Error::rejected(
            "hosted issue read did not return a revision; nothing was written",
        ));
    };
    if expected != rev {
        return Err(Error::conflict_coded(
            "revision_conflict",
            format!("issue revision changed to {rev}; read the latest body and retry"),
        ));
    }
    let Some(body) = current.get("body").and_then(Value::as_str) else {
        return Err(Error::rejected(
            "hosted issue read did not return a body; nothing was written",
        ));
    };
    parse::replace_acceptance(body, items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosted_acceptance_replaces_only_the_checklist_on_the_requested_revision() {
        let shown = json!({
            "id": "FIX-1",
            "rev": "wrev_body_fixture",
            "body": "## Outcome\n\nKeep x=y.\n\n## Acceptance\n\n- [ ] superseded\n\n## Notes\n\nKeep this.\n",
        });
        let items = parse::parse_acceptance_input("- [x] Check the revised contract").unwrap();
        assert_eq!(
            replacement(&shown, "wrev_body_fixture", &items).unwrap(),
            "## Outcome\n\nKeep x=y.\n\n## Acceptance\n\n- [x] Check the revised contract\n\n## Notes\n\nKeep this.\n",
        );
        let refused = replacement(&shown, "wrev_older", &items).unwrap_err();
        assert_eq!(refused.code(), Some("revision_conflict"));
    }

    #[test]
    fn hosted_body_file_matches_tenant_issue_set_contract() {
        // Same fixture as agenticos-v2's AOS-502 CLI body-patch acceptance:
        // indentation, internal `=`, and the trailing spaces must survive.
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            "  ## Acceptance\n\n  retain whitespace = exact\nnext=two  \n",
        )
        .unwrap();
        let action = IssueAction::Edit {
            id: "FIX-1".into(),
            set: vec![],
            tag: vec![],
            link: vec![],
            unlink: vec![],
            refs: vec![],
            unrefs: vec![],
            attach: vec![],
            body: Some(file.path().into()),
            acceptance: None,
            comment_file: None,
            author: None,
            force: None,
            if_rev: Some("wrev_body_fixture".into()),
        };
        assert_eq!(
            Value::Object(edit_args(&action).unwrap().unwrap()),
            json!({
                "ids": ["FIX-1"],
                "if_rev": "wrev_body_fixture",
                "set": ["body=  ## Acceptance\n\n  retain whitespace = exact\nnext=two  \n"]
            }),
        );
    }
}
