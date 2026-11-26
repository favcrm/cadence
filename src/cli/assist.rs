//! CAD-2832: `cadence assist` — personal assistant working memory, rich
//! `SOUL.md` updates, and permitted skill reads through the local daemon.

use super::*;
use std::io::Read;

const INPUT_CAP: u64 = 64 * 1024;

#[derive(Subcommand)]
pub(crate) enum AssistAction {
    /// Update this personal assistant's SOUL.md working norms (interactive turns only).
    Soul {
        #[command(subcommand)]
        action: AssistSoulAction,
    },
    /// Read, search, add, forget, or undo personal MEMORY.md and memory/<topic>.md entries.
    Memory {
        #[command(subcommand)]
        action: AssistMemoryAction,
    },
    /// Read a permitted skill's manifest or exact file on demand.
    Skill {
        #[command(subcommand)]
        action: AssistSkillAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum AssistSoulAction {
    /// Replace the multi-line SOUL.md working norms body (and optionally the short tone summary).
    Set {
        /// Read SOUL.md body from a file path, or `-` for stdin.
        #[arg(long, conflicts_with = "text")]
        file: Option<String>,
        /// Inline SOUL.md body text.
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
        /// Optional one-line tone summary.
        #[arg(long)]
        tone: Option<String>,
        /// Optional current SOUL.md SHA-256 for optimistic concurrency check.
        #[arg(long)]
        expected_sha: Option<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum AssistMemoryAction {
    /// Add one dated, single-line fact to MEMORY.md.
    Add {
        /// Concise fact text (<= 512 bytes, no secrets).
        fact: String,
    },
    /// Remove matching fact line(s) from MEMORY.md (interactive turns only).
    Forget {
        /// Substring or exact fact to match and remove.
        query: String,
    },
    /// Write or replace `memory/<topic>.md`.
    Topic {
        /// Topic slug (`[a-z0-9][a-z0-9-]{0,47}`).
        topic: String,
        /// Read topic Markdown from a file path, or `-` for stdin.
        #[arg(long, conflicts_with = "text")]
        file: Option<String>,
        /// Inline topic Markdown text.
        #[arg(long, conflicts_with = "file")]
        text: Option<String>,
    },
    /// Search across `MEMORY.md`, `memory/*.md`, and `memory/journal/*.md`.
    Search {
        /// Keywords to search for.
        query: String,
        /// Maximum number of line hits (clamped 1..=20).
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    /// Read one allowed personal memory/profile file (`MEMORY.md`, `SOUL.md`, `memory/<topic>.md`).
    Read {
        /// Relative file path inside the assistant's personal cwd.
        file: String,
    },
    /// Roll back one recorded memory/SOUL change by history ID (interactive turns only).
    Undo {
        /// History entry id (`mem_...`).
        history_id: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum AssistSkillAction {
    /// Read a permitted skill manifest, or `--file <path>` for one file's text.
    Show {
        /// Skill name, for example `assistant-memory`.
        name: String,
        /// Optional relative file path inside the skill bundle.
        #[arg(long)]
        file: Option<String>,
    },
}

fn read_text_arg(file: Option<String>, text: Option<String>, label: &str) -> Result<String> {
    match (file, text) {
        (Some(path), None) => {
            let mut buf = Vec::new();
            if path == "-" {
                std::io::stdin().take(INPUT_CAP + 1).read_to_end(&mut buf)?;
            } else {
                std::fs::File::open(&path)
                    .map_err(|e| Error::rejected(format!("{label}: cannot read {path}: {e}")))?
                    .take(INPUT_CAP + 1)
                    .read_to_end(&mut buf)?;
            }
            if buf.len() as u64 > INPUT_CAP {
                return Err(Error::rejected(format!(
                    "{label}: input exceeds {INPUT_CAP} bytes"
                )));
            }
            String::from_utf8(buf)
                .map_err(|_| Error::rejected(format!("{label}: input is not UTF-8")))
        }
        (None, Some(inline)) => Ok(inline),
        _ => Err(Error::usage(format!(
            "{label} requires exactly one of --file or --text"
        ))),
    }
}

pub(super) fn run(state_dir: PathBuf, action: AssistAction) -> Result<i32> {
    match action {
        AssistAction::Soul {
            action:
                AssistSoulAction::Set {
                    file,
                    text,
                    tone,
                    expected_sha,
                },
        } => {
            let soul = read_text_arg(file, text, "assist soul set")?;
            let mut params = json!({
                "op": "update_soul",
                "soul": soul,
            });
            if let Some(tone) = tone {
                params["tone"] = json!(tone);
            }
            if let Some(sha) = expected_sha {
                params["expected_sha"] = json!(sha);
            }
            let out = client::rpc(&state_dir, "assistant_context_update", params)?;
            print_json(&out);
        }
        AssistAction::Memory { action } => match action {
            AssistMemoryAction::Add { fact } => {
                let out = client::rpc(
                    &state_dir,
                    "assistant_context_update",
                    json!({"op": "remember", "fact": fact}),
                )?;
                print_json(&out);
            }
            AssistMemoryAction::Forget { query } => {
                let out = client::rpc(
                    &state_dir,
                    "assistant_context_update",
                    json!({"op": "forget", "query": query}),
                )?;
                print_json(&out);
            }
            AssistMemoryAction::Topic { topic, file, text } => {
                let content = read_text_arg(file, text, "assist memory topic")?;
                let out = client::rpc(
                    &state_dir,
                    "assistant_context_update",
                    json!({"op": "write_topic", "topic": topic, "content": content}),
                )?;
                print_json(&out);
            }
            AssistMemoryAction::Search { query, limit } => {
                let out = client::rpc(
                    &state_dir,
                    "assistant_memory_search",
                    json!({"query": query, "limit": limit}),
                )?;
                print_json(&out);
            }
            AssistMemoryAction::Read { file } => {
                let out = client::rpc(&state_dir, "assistant_memory_read", json!({"file": file}))?;
                print_json(&out);
            }
            AssistMemoryAction::Undo { history_id } => {
                let out = client::rpc(
                    &state_dir,
                    "assistant_memory_undo",
                    json!({"history_id": history_id}),
                )?;
                print_json(&out);
            }
        },
        AssistAction::Skill {
            action: AssistSkillAction::Show { name, file },
        } => {
            let mut params = json!({"name": name});
            if let Some(file) = file {
                params["file"] = json!(file);
            }
            let out = client::rpc(&state_dir, "assistant_skill_show", params)?;
            if let Some(text) = out.get("text").and_then(Value::as_str) {
                print_text(text);
            } else {
                print_json(&out);
            }
        }
    }
    Ok(0)
}
