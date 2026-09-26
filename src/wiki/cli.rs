//! `cadence wiki` — the CLI over the daemon's wiki RPCs (CAD-580).
//! Every call is a daemon RPC: the daemon derives the caller from the
//! connection, so the CLI never passes an identity — a `--by`-style
//! flag cannot exist here. Blob uploads stage the file under
//! `<state>/wiki-uploads/` (the only place `wiki_put_blob` accepts a
//! tmp from); `cat` on a blob reads the verified blob bytes directly
//! once the daemon's ACL check passes.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};

#[derive(Subcommand)]
pub enum WikiAction {
    /// List a wiki directory ("" or omitted = the root).
    Ls {
        /// Path inside the wiki — `global`, `projects/<key>/`,
        /// `agents/<alias>/knowledge`, `users/<u>/`.
        path: Option<String>,
    },
    /// Print a page. A blob page streams its bytes; `--meta` prints
    /// the pointer's JSON instead.
    Cat {
        path: String,
        /// Show the blob pointer (sha256/size/mime) rather than bytes.
        #[arg(long)]
        meta: bool,
    },
    /// Write a page: text from `-m`/`--file`/stdin, or a binary blob
    /// with `--blob --file`.
    Put {
        path: String,
        /// Page text.
        #[arg(short = 'm')]
        text: Option<String>,
        /// Read the page (or blob, with --blob) from a file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Upload the file as a content-addressed blob.
        #[arg(long)]
        blob: bool,
        /// Optimistic-concurrency token from `cat`'s rev — "none"
        /// requires the page to not exist yet.
        #[arg(long)]
        if_rev: Option<String>,
    },
    /// Create a directory.
    Mkdir { path: String },
    /// Rename a page or directory (`from` may be a `.trash/` path the
    /// operator restores).
    Mv { from: String, to: String },
    /// Move a page or directory to `.trash/` — the reply names the
    /// restore path.
    Rm { path: String },
    /// Case-insensitive substring search over wiki text files.
    Search {
        /// The query.
        q: String,
        /// Limit to a subtree (default: the whole wiki).
        #[arg(long)]
        path: Option<String>,
    },
    /// The git log for a page.
    History {
        path: String,
        /// Max commits (default 50).
        #[arg(long)]
        limit: Option<u64>,
    },
}

fn rpc(state_dir: &Path, method: &str, params: Value) -> Result<Value> {
    client::rpc(state_dir, method, params)
}

fn entries(out: &Value) {
    let path = out["path"].as_str().unwrap_or("");
    println!("{path}:");
    for e in out["entries"].as_array().cloned().unwrap_or_default() {
        let kind = e["kind"].as_str().unwrap_or("");
        let name = e["name"].as_str().unwrap_or("");
        let size = e["size"].as_u64();
        let mime = e["mime"].as_str().unwrap_or("");
        let suffix = match (kind, size, mime) {
            ("dir", _, _) => "/".to_string(),
            (_, Some(n), "") => format!("  ({n} B)"),
            (_, Some(n), m) => format!("  ({n} B, {m})"),
            _ => String::new(),
        };
        println!("  {name}{suffix}");
    }
}

/// `cadence wiki …` — returns the process exit code.
pub fn run(action: &WikiAction, state_dir: &Path) -> Result<i32> {
    match action {
        WikiAction::Ls { path } => {
            let out = rpc(
                state_dir,
                "wiki_ls",
                json!({"path": path.clone().unwrap_or_default()}),
            )?;
            entries(&out);
            Ok(0)
        }
        WikiAction::Cat { path, meta } => {
            let out = rpc(state_dir, "wiki_read", json!({"path": path}))?;
            match out["kind"].as_str() {
                Some("text") => {
                    print!("{}", out["text"].as_str().unwrap_or(""));
                    Ok(0)
                }
                Some("blob") if *meta => {
                    crate::issue::cli::print_json(&out);
                    Ok(0)
                }
                Some("blob") => {
                    let sha = out["sha256"].as_str().unwrap_or_default();
                    let blob = crate::issue::Pm::open_default().and_then(|pm| {
                        crate::wiki::vault_dir(&pm).map(|v| crate::wiki::blobs_dir(&v).join(sha))
                    })?;
                    let bytes = std::fs::read(&blob).map_err(|e| {
                        Error::rejected(format!("cannot read blob {}: {e}", blob.display()))
                    })?;
                    std::io::stdout().write_all(&bytes)?;
                    Ok(0)
                }
                Some("dir") => Err(Error::rejected(format!(
                    "wiki cat '{path}' refused: a directory — use `wiki ls`"
                ))),
                _ => Err(Error::rejected(format!("wiki cat '{path}': no such page"))),
            }
        }
        WikiAction::Put {
            path,
            text,
            file,
            blob,
            if_rev,
        } => {
            if *blob {
                let file = file
                    .as_ref()
                    .ok_or_else(|| Error::rejected("wiki put --blob needs --file <path>"))?;
                if text.is_some() {
                    return Err(Error::rejected("wiki put --blob takes --file, not -m"));
                }
                let uploads = state_dir.join(crate::wiki::UPLOAD_DIR);
                std::fs::create_dir_all(&uploads)?;
                let tmp = uploads.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
                let mut src = crate::master::open_command_file(state_dir, file)?;
                let mut dst = std::fs::File::create(&tmp)?;
                std::io::copy(&mut src, &mut dst).map_err(|e| {
                    Error::rejected(format!("wiki put: cannot stage {}: {e}", file.display()))
                })?;
                let out = rpc(
                    state_dir,
                    "wiki_put_blob",
                    json!({"path": path, "tmp": tmp, "if_rev": if_rev}),
                );
                let out = match out {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(e);
                    }
                };
                if out.get("conflict").is_some() {
                    crate::issue::cli::print_json(&out);
                    return Err(Error::rejected(format!(
                        "wiki put '{path}': if_rev conflict — re-read and retry"
                    )));
                }
                crate::issue::cli::print_json(&out);
                return Ok(0);
            }
            // The master does not pass `-m`. That channel is unbounded
            // and sits beside `--file`; content is a file inside
            // `<state>/master/tmp`. Other callers keep `-m` and stdin.
            if crate::master::caller_is_master() && text.is_some() {
                return Err(Error::rejected(
                    "the master does not pass -m — use the write tool into master/tmp, then --file <that path>",
                ));
            }
            if crate::master::caller_is_master()
                && file
                    .as_ref()
                    .is_none_or(|f| f.as_os_str() == "-" || f.as_os_str().is_empty())
            {
                return Err(Error::rejected(crate::master::NO_STDIN));
            }
            let text = match (text, file) {
                (Some(t), _) => t.clone(),
                (None, Some(f)) => crate::master::read_command_file(state_dir, f, u64::MAX)
                    .map_err(|e| {
                        Error::rejected(format!("wiki put: cannot read {}: {e}", f.display()))
                    })?,
                (None, None) => {
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            let out = rpc(
                state_dir,
                "wiki_write",
                json!({"path": path, "text": text, "if_rev": if_rev}),
            )?;
            if out.get("conflict").is_some() {
                crate::issue::cli::print_json(&out);
                return Err(Error::rejected(format!(
                    "wiki put '{path}': if_rev conflict — re-read and retry"
                )));
            }
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        WikiAction::Mkdir { path } => {
            let out = rpc(state_dir, "wiki_mkdir", json!({"path": path}))?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        WikiAction::Mv { from, to } => {
            let out = rpc(state_dir, "wiki_mv", json!({"from": from, "to": to}))?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        WikiAction::Rm { path } => {
            let out = rpc(state_dir, "wiki_rm", json!({"path": path}))?;
            crate::issue::cli::print_json(&out);
            Ok(0)
        }
        WikiAction::Search { q, path } => {
            let out = rpc(
                state_dir,
                "wiki_search",
                json!({"q": q, "path": path.clone().unwrap_or_default()}),
            )?;
            for m in out["matches"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{}:{}: {}",
                    m["path"].as_str().unwrap_or(""),
                    m["line"].as_u64().unwrap_or(0),
                    m["text"].as_str().unwrap_or("")
                );
            }
            Ok(0)
        }
        WikiAction::History { path, limit } => {
            let mut params = json!({"path": path});
            if let Some(n) = limit {
                params["limit"] = json!(n);
            }
            let out = rpc(state_dir, "wiki_history", params)?;
            for c in out["commits"].as_array().cloned().unwrap_or_default() {
                let actor = c["actor"].as_str().unwrap_or("");
                let at = c["at"].as_u64().unwrap_or(0);
                println!(
                    "{} {} {}",
                    &c["sha"].as_str().unwrap_or("")
                        [..7.min(c["sha"].as_str().map(str::len).unwrap_or(0))],
                    actor,
                    c["subject"].as_str().unwrap_or(""),
                );
                let _ = at;
            }
            Ok(0)
        }
    }
}
