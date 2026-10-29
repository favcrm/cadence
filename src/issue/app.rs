//! Apps (CAD-547): an app is a folder that bundles the workflows for
//! one kind of work — `app.md` (frontmatter `app`, `title`, `version`,
//! `needs.connections: [<slot>…]`; the body is the guide agents read),
//! `workflows/*.md` (the same plan templates CAD-487 ships), optional
//! flat `rubrics/` and `templates/` dirs, and — CAD-864 — an optional
//! `views/app-views-v1.json` data-only view descriptor declared by
//! `needs.views.contract`. `records`, `actions`, `ui`, `settings` stay
//! gated for later stages.
//!
//! `cadence app install <path|git-url> --project <key>` copies a
//! verified bundle to `<pm>/<project>/apps/<name>/` and writes the
//! install record beside it at `apps/<name>.yaml` — source (a git
//! install pins the commit SHA), when, by whom, and each slot's
//! binding. An installed app lands **unapproved**: nothing in it runs
//! until the operator's `cadence app approve` records its structural
//! digest, the same gate-and-digest model `workflow approve` uses.
//!
//! The digest covers what the operator reviewed: the app name, the
//! declared slots, each slot's effective binding (an `app set` rebind
//! is a structural change — it re-gates), the `app.md` guide body, each
//! workflow's CAD-487 gate digest (its wording stays wording-free,
//! exactly like a stored workflow), and a content hash of every
//! rubric/template file. A wording-only workflow edit keeps approval;
//! a slot, binding, guide, rubric or template change does not —
//! `plan propose --workflow <app>/<wf>` refuses `app_unapproved` until
//! the operator re-approves.
//!
//! Slots: `needs.connections` declares names; each binds to a
//! connection name (`cadence app set <app> <slot>=<connection>`),
//! defaulting to the built-in `local` (CAD-546's connector — the name
//! is the contract, not its implementation). A workflow step names the
//! slot in `uses:` — never a service directly — and install refuses a
//! `uses:` the app did not declare.
//!
//! Install is safe on hostile input: the bundle is never executed, no
//! symlinks are followed (any link refuses), only the known top-level
//! entries are read, every file is bounded UTF-8 text, and a git
//! source is cloned into a temp dir and pinned to its HEAD SHA before
//! any byte is looked at.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::model;
use crate::issue::parse;
use crate::issue::{app_view, board, plan, project, workflow, write, Pm};

use cadence_core::app_compat::{self, Compat};

/// `<pm>/<project>/apps/` — beside PROJECT.md and `workflows/`.
pub const DIR: &str = "apps";

/// `app.md` — the one required file of an app folder.
pub const MANIFEST: &str = "app.md";

/// The built-in connection a slot binds when nothing else is recorded —
/// CAD-546's local connector. The name is the contract; whether the
/// connector is registered yet is a `doctor` finding, not an install
/// refusal.
pub const LOCAL_CONNECTION: &str = "local";

/// `app.md` frontmatter keys v0 knows. Anything else refuses — the same
/// fail-loud rule the workflow and plan parsers apply.
const MANIFEST_KEYS: &[&str] = &["app", "title", "version", "needs", "summary", "listing"];

/// Frontmatter keys the later stages reserve — refused with their own
/// message so a bundle carrying one names the stage, not "unknown key".
const GATED_KEYS: &[&str] = &["records", "actions", "ui", "settings", "actors"];

/// The only directories an app folder may carry at top level. CAD-1006:
/// `screens/` joins them — it holds one `<tag>/` subdir per screen
/// package, each carrying a `screens.json` declaration plus flat
/// `<stem>.<js|css|svg|json>` asset leaves. `views/` (CAD-864) holds
/// exactly one file — `views/app-views-v1.json`, the app-views/v1
/// descriptor — and only when `app.md` declares `needs.views.contract`.
/// CAD-1129: `assets/` is the catalog's icon and screenshots — flat
/// `*.svg` files only. CAD-1867: `skills/` holds one `<tag>/` package per
/// installed skill — its `skill.json` manifest, flat root text leaves, and
/// `scripts/` and `references/` subdirectories.
const TOP_DIRS: &[&str] = &[
    "workflows",
    "rubrics",
    "templates",
    "screens",
    "views",
    "assets",
    "storage",
    "skills",
];

/// Largest single file in a bundle — workflows render to plans, so the
/// plan cap applies; the same bound keeps every other file small.
const MAX_FILE_BYTES: u64 = plan::MAX_PLAN_BYTES as u64;

/// CAD-1254: a declared screen's JS/CSS asset (`screens/<tag>/<stem>.js|css`)
/// gets its own 384 KiB per-file cap; nothing else is raised.
pub(crate) const MAX_SCREEN_ASSET_BYTES: u64 = 384 * 1024;

/// The per-file cap for bundle member `rel`: [`MAX_SCREEN_ASSET_BYTES`] for a
/// screen JS/CSS leaf, the skill caps inside a `skills/<tag>/` package, else
/// [`MAX_FILE_BYTES`].
pub(crate) fn file_cap(rel: &str) -> u64 {
    let screen_leaf = rel
        .strip_prefix("screens/")
        .is_some_and(|r| r.split('/').count() == 2 && (r.ends_with(".js") || r.ends_with(".css")));
    if screen_leaf {
        MAX_SCREEN_ASSET_BYTES
    } else if rel
        .strip_prefix("skills/")
        .is_some_and(|r| r.split('/').count() == 2 && r.ends_with(&format!("/{SKILL_MANIFEST}")))
    {
        SKILL_MANIFEST_BYTES
    } else if rel.starts_with("skills/") {
        SKILL_FILE_BYTES
    } else {
        MAX_FILE_BYTES
    }
}

/// The root files admitted beside `app.md`. One list for the source loader,
/// the catalogue importer and the board's files-mode transport. CAD-1415 added
/// `app-actions.json` to the first two and missed the third, so a bundle the
/// daemon accepts was refused before it ever reached the daemon.
pub(crate) const ROOT_MEMBERS: &[&str] = &[
    MANIFEST,
    crate::issue::app_chat::FILE,
    crate::issue::app_assistant::FILE,
    crate::issue::app_action::FILE,
    "app-storage.json",
];

pub(crate) fn root_member(name: &str) -> bool {
    ROOT_MEMBERS.contains(&name)
}

/// Visible, ASCII, flat leaf name — no dotfile, no separator, no escape.
fn plain_leaf_ok(seg: &str) -> bool {
    !seg.is_empty()
        && seg.is_ascii()
        && !seg.starts_with('.')
        && seg != ".."
        && !seg.contains('\\')
        && !seg.contains('\0')
}

/// A workflow leaf: `<tag>.md`, the installer's own tag rule. Every trailing
/// `.md` is stripped before the tag check, which the published document states
/// explicitly.
fn workflow_leaf_ok(seg: &str) -> bool {
    plain_leaf_ok(seg)
        && seg.ends_with(WORKFLOW_SUFFIX)
        && crate::issue::model::valid_tag(seg.trim_end_matches(WORKFLOW_SUFFIX))
}

/// How one flat top-level directory admits its leaves.
#[derive(Clone, Copy)]
pub(crate) enum DirRule {
    /// `<tag>.md`, the installer's own tag rule.
    WorkflowMdTag,
    /// One tag-shaped stem and exactly one declared suffix.
    TaggedLeaf { suffix: &'static str },
    /// Exactly this leaf name.
    Exact(&'static str),
    /// Any visible, ASCII, flat leaf.
    VisibleLeaf,
    /// A visible flat `<leaf>.svg`, bounded in bytes.
    SvgLeaf { max_leaf_bytes: usize },
    /// A `screens/<tag>/` package: the declaration or a tag-shaped stem plus
    /// `js|css|svg|json`.
    ScreensPackage,
    /// A `skills/<tag>/` package: its `skill.json` manifest, flat root text
    /// leaves, and `scripts/` and `references/` subdirectories. See
    /// [`skill_member_ok`].
    SkillsPackage,
}

/// The directory half of the grammar, in one table. [`bundle_path_ok`]
/// evaluates it and [`bundle_grammar_json`] publishes it; there is no second
/// list to keep in step.
pub(crate) const DIR_RULES: &[(&str, DirRule)] = &[
    ("workflows", DirRule::WorkflowMdTag),
    ("views", DirRule::Exact(crate::issue::app_view::FILE)),
    ("rubrics", DirRule::VisibleLeaf),
    ("templates", DirRule::VisibleLeaf),
    // CAD-1129: catalog icons and screenshots are flat SVG leaves.
    (
        "assets",
        DirRule::SvgLeaf {
            max_leaf_bytes: ASSET_MAX_LEAF_BYTES,
        },
    ),
    ("screens", DirRule::ScreensPackage),
    // CAD-1867: an installed workflow's skill material — one
    // `<tag>/` package per skill, frozen into a run at creation.
    ("skills", DirRule::SkillsPackage),
    ("storage", DirRule::TaggedLeaf { suffix: ".json" }),
];

/// Version of the published grammar document. Bump only for a shape change.
#[cfg(test)]
pub(crate) const GRAMMAR_VERSION: u32 = 3;

/// The lexical pieces the published document must state normatively. They are
/// constants so the predicate and the document cannot disagree.
const WORKFLOW_SUFFIX: &str = ".md";
const ASSET_SUFFIX: &str = ".svg";
const ASSET_MAX_LEAF_BYTES: usize = 128;
#[cfg(test)]
const SCREEN_ASSET_EXTS: &[&str] = &["js", "css", "svg", "json"];
/// `model::valid_tag`: every byte drawn from this set, 1..=32 bytes, never
/// leading with `-`. Stated as a charset, not a regex: `$` admits a trailing
/// line terminator in JavaScript, which would let a consumer accept a tag the
/// host refuses.
#[cfg(test)]
const VALID_TAG_CHARSET: &str = "abcdefghijklmnopqrstuvwxyz0123456789-";
#[cfg(test)]
const VALID_TAG_MAX_BYTES: usize = 32;

fn dir_rule(directory: &str) -> Option<DirRule> {
    DIR_RULES
        .iter()
        .find(|(name, _)| *name == directory)
        .map(|(_, rule)| *rule)
}

/// CAD-1867: the skill package directory and its manifest. A package is
/// `skills/<tag>/`; `skill.json` declares which members it carries.
pub const SKILL_DIR: &str = "skills";
pub const SKILL_MANIFEST: &str = "skill.json";
/// The one member a package must always declare: the instructions an agent
/// reads first.
pub const SKILL_ENTRY: &str = "SKILL.md";
/// A package declares at most this many members (its manifest excluded).
pub const SKILL_FILES_MAX: usize = 16;
/// Largest declared member, and largest `skill.json`.
pub const SKILL_FILE_BYTES: u64 = 16 * 1024;
pub const SKILL_MANIFEST_BYTES: u64 = 4 * 1024;
/// Largest package: `skill.json` plus every declared member.
pub const SKILL_PACKAGE_BYTES: u64 = 32 * 1024;

/// Extensions a package's flat root leaves may carry.
const SKILL_ROOT_EXTS: &[&str] = &["md", "txt", "json", "yaml", "yml", "csv", "css", "svg"];
/// Extensions the one nested level admits, per subdirectory. `scripts/` holds
/// code an agent may run; `references/` holds prose and notes.
const SKILL_SCRIPTS_EXTS: &[&str] = &["py", "sh", "js", "mjs"];
const SKILL_REFERENCES_EXTS: &[&str] = &["md", "txt"];
/// Every extension the reference scan treats as naming package material.
const SKILL_ALL_EXTS: &[&str] = &[
    "md", "txt", "json", "yaml", "yml", "csv", "css", "svg", "py", "sh", "js", "mjs",
];

/// `scripts/` and `references/` are the only nested directories, each exactly
/// one level deep.
fn skill_nested_ext(directory: &str) -> Option<&'static [&'static str]> {
    match directory {
        "scripts" => Some(SKILL_SCRIPTS_EXTS),
        "references" => Some(SKILL_REFERENCES_EXTS),
        _ => None,
    }
}

/// Whether `directory` is one of the package's two nested directories. The
/// bundle walkers ask this before descending, so `skills/<tag>/<dir>/` can only
/// ever be `scripts/` or `references/`.
pub(crate) fn skill_subdir_ok(directory: &str) -> bool {
    skill_nested_ext(directory).is_some()
}

fn skill_leaf_ok(name: &str, exts: &[&str]) -> bool {
    plain_leaf_ok(name)
        && exts.iter().any(|ext| {
            name.strip_suffix(&format!(".{ext}"))
                .is_some_and(|stem| !stem.is_empty())
        })
}

/// The grammar of one package-relative member: a flat root leaf with an
/// allowed text extension, or `<scripts|references>/<leaf>` with that
/// directory's extensions. Nothing deeper, no dotfiles, no separators.
pub fn skill_member_ok(member: &str) -> bool {
    if member.is_empty() || !member.is_ascii() || member.contains('\\') || member.contains('\0') {
        return false;
    }
    let segs: Vec<&str> = member.split('/').collect();
    match segs.as_slice() {
        [leaf] => skill_leaf_ok(leaf, SKILL_ROOT_EXTS),
        [directory, leaf] => {
            skill_nested_ext(directory).is_some_and(|exts| skill_leaf_ok(leaf, exts))
        }
        _ => false,
    }
}

/// The `skill.json` a package must carry. Closed decode: an unknown field
/// refuses, so a package cannot smuggle a contract this host does not read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillManifest {
    pub schema: u32,
    pub name: String,
    /// Every member of the package except `skill.json` itself, in declared
    /// order. The set must equal the set actually present — both directions
    /// refuse, naming the path.
    pub files: Vec<String>,
}

/// One declared member, frozen: the exact bytes the host read, and their
/// digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillFile {
    pub path: String,
    pub sha256: String,
    pub text: String,
}

/// A skill package frozen at run creation: `{name, manifest,
/// manifest_sha256, digest, files}`. This value travels in the run snapshot
/// and in the dispatch kickoff and nowhere else — the host materializes no
/// copy on disk, so a later bundle update cannot change what a run was given.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPackage {
    pub name: String,
    pub manifest: SkillManifest,
    pub manifest_sha256: String,
    pub digest: String,
    pub files: Vec<SkillFile>,
}

/// The one skill manifest validator: install validation and run creation both
/// reach a package through [`skill_package`], which is the only caller of
/// this, so the two can never disagree about what a complete package is.
///
/// `tag` is the package's directory name. The manifest's own `name` must be
/// that tag — a package cannot declare itself to be another one.
pub fn parse_skill_manifest(text: &str, tag: &str) -> Result<SkillManifest> {
    if !crate::issue::model::valid_tag(tag) {
        return Err(Error::rejected(format!(
            "skill package '{tag}' — a package name is [a-z0-9-], ≤32 bytes"
        )));
    }
    if text.len() as u64 > SKILL_MANIFEST_BYTES {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} is {} bytes — a skill manifest is at \
             most {SKILL_MANIFEST_BYTES}",
            text.len()
        )));
    }
    let manifest: SkillManifest = serde_json::from_str(text).map_err(|e| {
        Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST}: {e} — the manifest is \
             {{\"schema\":1,\"name\":\"{tag}\",\"files\":[\"{SKILL_ENTRY}\"]}}"
        ))
    })?;
    if manifest.schema != 1 {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST}: schema {} — this host reads schema 1",
            manifest.schema
        )));
    }
    if manifest.name != tag {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares name '{}' — it must be its \
             own directory name '{tag}'",
            manifest.name
        )));
    }
    if manifest.files.is_empty() {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares no files — it must declare \
             {SKILL_ENTRY} and every member the package carries"
        )));
    }
    if manifest.files.len() > SKILL_FILES_MAX {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag} declares {} files — a package declares at most \
             {SKILL_FILES_MAX}",
            manifest.files.len()
        )));
    }
    for (i, member) in manifest.files.iter().enumerate() {
        if member == SKILL_MANIFEST {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} lists '{SKILL_MANIFEST}' — the \
                 manifest is never one of its own declared files"
            )));
        }
        if !skill_member_ok(member) {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares '{member}' — a package \
                 member is a flat root <stem>.<{}>, or scripts/<stem>.<{}>, or \
                 references/<stem>.<{}>",
                SKILL_ROOT_EXTS.join("|"),
                SKILL_SCRIPTS_EXTS.join("|"),
                SKILL_REFERENCES_EXTS.join("|"),
            )));
        }
        if manifest.files[..i].contains(member) {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares '{member}' twice"
            )));
        }
    }
    if !manifest.files.iter().any(|member| member == SKILL_ENTRY) {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} must declare {SKILL_ENTRY} — the \
             instructions an agent reads first"
        )));
    }
    Ok(manifest)
}

/// The digest over a frozen package: the name, the manifest bytes and every
/// declared member's path and digest, length-prefixed in declared order.
fn skill_digest(name: &str, manifest_sha256: &str, files: &[SkillFile]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"cadence-app-skill-v1\n");
    for part in [name, manifest_sha256] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    for file in files {
        for part in [file.path.as_str(), file.sha256.as_str()] {
            digest.update((part.len() as u64).to_be_bytes());
            digest.update(part.as_bytes());
        }
    }
    format!("sha256:{:x}", digest.finalize())
}

/// Tokens in `text` that could name package material. A token is a run of
/// ASCII alphanumerics plus `.`, `_`, `/` and `-`; everything else is a
/// separator. Every token is returned: dropping a long one would be a
/// reference this guard silently ignores, and the files are bounded anyway.
fn reference_tokens(text: &str) -> impl Iterator<Item = &str> + '_ {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-')))
        .filter(|token| !token.is_empty())
}

/// The package-relative path a token names, or `None` when the token names
/// nothing this contract tracks.
///
/// Every `.`-led spelling is the same relative path: `./x`, `.//x` and
/// `.//./x` all name `x`, and `.`/empty segments carry no meaning. Only a token
/// that starts with a slash and was never `.`-led is an absolute path, which is
/// somebody else's file. `..` would escape the package and refuses.
fn referenced_member(token: &str, tag: &str) -> Option<Reference> {
    // A sentence-ending period is a separator, not part of a member name:
    // `run ./scripts/build.py.` names `scripts/build.py`.
    let token = token.trim_end_matches('.');
    if token.is_empty() {
        return None;
    }
    let absolute = token.starts_with('/') && !token.starts_with('.');
    let mut escaped = false;
    let mut segments: Vec<&str> = Vec::new();
    for segment in token.split('/') {
        match segment {
            "" | "." => continue,
            // `..` cannot be resolved against the package, so a token that
            // uses it and still names package material refuses below rather
            // than being dropped as if it named nothing.
            ".." => escaped = true,
            other => segments.push(other),
        }
    }
    if segments.is_empty() || absolute {
        return None;
    }
    let path = segments.join("/");
    if let Some(rest) = path.strip_prefix(&format!("{SKILL_DIR}/")) {
        let (owner, member) = rest.split_once('/')?;
        return Some(Reference {
            owner: owner.to_string(),
            member: member.to_string(),
            escaped,
        });
    }
    for directory in ["scripts", "references"] {
        if path.starts_with(&format!("{directory}/")) {
            return Some(Reference {
                owner: tag.to_string(),
                member: path,
                escaped,
            });
        }
    }
    if path.contains('/') {
        return None;
    }
    let ext = path.rsplit_once('.').map(|(_, ext)| ext)?;
    if !SKILL_ALL_EXTS.contains(&ext) {
        return None;
    }
    Some(Reference {
        owner: tag.to_string(),
        member: path,
        escaped,
    })
}

/// One token that names package material: the package it names (`skills/` may
/// name another package, which refuses) and the member path inside it.
/// `escaped` records a `..` segment, which cannot be resolved inside the
/// package and therefore refuses instead of being ignored.
struct Reference {
    owner: String,
    member: String,
    escaped: bool,
}

/// CAD-1867: the declared-inventory check. Every declared text file is
/// scanned for tokens that name package material, and a token naming a member
/// the manifest does not declare refuses.
///
/// This is a declared-inventory contract, NOT import analysis: it reads names,
/// never evaluates them, so a path assembled at run time (`os.path.join`,
/// `importlib`, a variable) is invisible to it. It fails closed on the names it
/// can see — including a bare `name.ext` with an allowed extension, which is
/// read as a package member — because a silently ignored reference is a member
/// an agent would look for and not find.
fn undeclared_references(
    files: &[SkillFile],
    declared: &BTreeSet<String>,
    tag: &str,
) -> Vec<String> {
    let mut refusals: Vec<String> = Vec::new();
    for file in files {
        for token in reference_tokens(&file.text) {
            let Some(reference) = referenced_member(token, tag) else {
                continue;
            };
            let refusal = if reference.escaped {
                format!(
                    "{SKILL_DIR}/{tag}/{path}: '{token}' reaches package material through '..' — \
                     name the member path inside the package",
                    path = file.path
                )
            } else if reference.owner != tag {
                format!(
                    "{SKILL_DIR}/{tag}/{path}: '{token}' names skill package '{owner}' — a \
                     package references only its own declared members",
                    path = file.path,
                    owner = reference.owner
                )
            } else if declared.contains(&reference.member) {
                continue;
            } else {
                format!(
                    "{SKILL_DIR}/{tag}/{path}: '{token}' is not declared in \
                     {SKILL_DIR}/{tag}/{SKILL_MANIFEST} — declare '{member}' in its files",
                    path = file.path,
                    member = reference.member
                )
            };
            if !refusals.contains(&refusal) {
                refusals.push(refusal);
            }
        }
    }
    refusals
}

/// Resolve ONE skill package out of a bundle's file map — `rel-path → text`.
///
/// This is the only package validator: install validation runs it over every
/// package a bundle carries and over every tag a workflow names, and run
/// creation runs it over the tag it freezes, so the two cannot disagree. The
/// map is the bundle's own bytes (the installed file map at run creation);
/// nothing here touches the filesystem.
pub fn skill_package(files: &BTreeMap<String, String>, tag: &str) -> Result<SkillPackage> {
    if !crate::issue::model::valid_tag(tag) {
        return Err(Error::rejected(format!(
            "skill '{tag}' — a skill name is [a-z0-9-], ≤32 bytes"
        )));
    }
    let prefix = format!("{SKILL_DIR}/{tag}/");
    let manifest_path = format!("{prefix}{SKILL_MANIFEST}");
    let manifest_text = files.get(&manifest_path).ok_or_else(|| {
        Error::rejected(format!(
            "no skill package '{tag}' in this bundle — it needs {manifest_path}"
        ))
    })?;
    let manifest = parse_skill_manifest(manifest_text, tag)?;
    // What the bundle actually carries under this package.
    let mut present: BTreeSet<String> = BTreeSet::new();
    for rel in files.keys().filter_map(|rel| rel.strip_prefix(&prefix)) {
        if !skill_member_ok(rel) {
            return Err(Error::rejected(format!(
                "{prefix}{rel} — a package member is a flat root <stem>.<{}>, or \
                 scripts/<stem>.<{}>, or references/<stem>.<{}>; nothing deeper, no \
                 dotfiles",
                SKILL_ROOT_EXTS.join("|"),
                SKILL_SCRIPTS_EXTS.join("|"),
                SKILL_REFERENCES_EXTS.join("|"),
            )));
        }
        present.insert(rel.to_string());
    }
    // The manifest was loaded from this same map before the scan. It is a
    // root text leaf, so the scan above keeps it in `present`; the declared
    // set deliberately excludes it.
    let declared: BTreeSet<String> = manifest.files.iter().cloned().collect();
    for member in &declared {
        if !present.contains(member) {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{tag}/{SKILL_MANIFEST} declares '{member}' but the bundle \
                 does not carry {prefix}{member} — a package declares exactly what it \
                 ships"
            )));
        }
    }
    for member in &present {
        if member != SKILL_MANIFEST && !declared.contains(member) {
            return Err(Error::rejected(format!(
                "{prefix}{member} is not declared in {SKILL_DIR}/{tag}/{SKILL_MANIFEST} \
                 — declare it in the manifest's files, or remove it"
            )));
        }
    }
    let mut total = manifest_text.len() as u64;
    let mut frozen: Vec<SkillFile> = Vec::with_capacity(manifest.files.len());
    for member in &manifest.files {
        let text = files.get(&format!("{prefix}{member}")).ok_or_else(|| {
            Error::internal(format!(
                "{prefix}{member} vanished after the presence check"
            ))
        })?;
        if text.len() as u64 > SKILL_FILE_BYTES {
            return Err(Error::rejected(format!(
                "{prefix}{member} is {} bytes — a skill member is at most {SKILL_FILE_BYTES}",
                text.len()
            )));
        }
        total += text.len() as u64;
        frozen.push(SkillFile {
            path: member.clone(),
            sha256: format!("sha256:{}", sha256_hex(text.as_bytes())),
            text: text.clone(),
        });
    }
    if total > SKILL_PACKAGE_BYTES {
        return Err(Error::rejected(format!(
            "{SKILL_DIR}/{tag} is {total} bytes — a skill package is at most \
             {SKILL_PACKAGE_BYTES} including its manifest"
        )));
    }
    let refusals = undeclared_references(&frozen, &declared, tag);
    if !refusals.is_empty() {
        return Err(Error::rejected(refusals.join("; ")));
    }
    let manifest_sha256 = format!("sha256:{}", sha256_hex(manifest_text.as_bytes()));
    Ok(SkillPackage {
        name: tag.to_string(),
        digest: skill_digest(tag, &manifest_sha256, &frozen),
        manifest,
        manifest_sha256,
        files: frozen,
    })
}

/// Every skill package a bundle carries, by tag.
pub fn skill_packages(files: &BTreeMap<String, String>) -> Result<Vec<SkillPackage>> {
    let mut tags: Vec<&str> = Vec::new();
    for rel in files.keys() {
        let Some(rest) = rel.strip_prefix(&format!("{SKILL_DIR}/")) else {
            continue;
        };
        let Some((tag, _)) = rest.split_once('/') else {
            return Err(Error::rejected(format!(
                "{SKILL_DIR}/{rest} — a skill package is a {SKILL_DIR}/<tag>/ directory"
            )));
        };
        if !tags.contains(&tag) {
            tags.push(tag);
        }
    }
    tags.into_iter()
        .map(|tag| skill_package(files, tag))
        .collect()
}

/// The one bundle path grammar. The board's files-mode transport (install and
/// upgrade) admits every member through this, and the source loader and the
/// catalogue importer share [`root_member`] for the root set while keeping
/// their own directory walkers. The root list is therefore one fact, and the
/// transport can no longer refuse a member the daemon accepts.
///
/// It is lexical only: it says which paths may exist, never whether their bytes
/// are valid. Splitting on literal `/` and refusing empty, `.` and `..`
/// segments matters for the upload transport: `Path::components` folds `a//b`,
/// `a/./b` and `a/../b` onto one staged file and would defeat duplicate
/// detection.
pub(crate) fn bundle_path_ok(path: &str) -> bool {
    if path.is_empty() || !path.is_ascii() || path.contains('\\') || path.contains('\0') {
        return false;
    }
    let segs: Vec<&str> = path.split('/').collect();
    if segs.iter().any(|s| s.is_empty() || *s == "." || *s == "..") {
        return false;
    }
    match segs.as_slice() {
        [name] => root_member(name),
        [directory, leaf] => match dir_rule(directory) {
            Some(DirRule::WorkflowMdTag) => workflow_leaf_ok(leaf),
            Some(DirRule::TaggedLeaf { suffix }) => {
                plain_leaf_ok(leaf)
                    && leaf
                        .strip_suffix(suffix)
                        .is_some_and(crate::issue::model::valid_tag)
            }
            Some(DirRule::Exact(name)) => *leaf == name,
            Some(DirRule::VisibleLeaf) => plain_leaf_ok(leaf),
            Some(DirRule::SvgLeaf { max_leaf_bytes }) => {
                plain_leaf_ok(leaf) && leaf.ends_with(ASSET_SUFFIX) && leaf.len() <= max_leaf_bytes
            }
            // A screens package is a directory, never a flat leaf.
            Some(DirRule::ScreensPackage) | Some(DirRule::SkillsPackage) | None => false,
        },
        // CAD-1006: one tag-named package dir per screen.
        ["screens", tag, leaf] => {
            crate::issue::model::valid_tag(tag) && crate::issue::app_screen_pkg::leaf_ok(leaf)
        }
        // CAD-1867: one tag-named package dir per skill — a flat root leaf or
        // a file in `scripts/` or `references/`.
        ["skills", tag, member] => crate::issue::model::valid_tag(tag) && skill_member_ok(member),
        ["skills", tag, directory, leaf] => {
            crate::issue::model::valid_tag(tag) && skill_member_ok(&format!("{directory}/{leaf}"))
        }
        _ => false,
    }
}

/// The published grammar document, generated from [`ROOT_MEMBERS`],
/// [`DIR_RULES`] and the caps. `contracts/bundle-grammar/v3/grammar.json` must
/// equal this byte for byte; the test below refuses drift. The v1 and v2
/// documents stay as the historical artifacts a v1/v2 reader already carries.
#[cfg(test)]
pub(crate) fn bundle_grammar_json() -> String {
    let directories: Vec<serde_json::Value> = DIR_RULES
        .iter()
        .map(|(directory, rule)| match rule {
            DirRule::WorkflowMdTag => {
                serde_json::json!({ "dir": directory, "rule": "workflow_md_tag" })
            }
            DirRule::TaggedLeaf { suffix } => {
                serde_json::json!({ "dir": directory, "rule": "tagged_leaf", "suffix": suffix })
            }
            DirRule::Exact(name) => {
                serde_json::json!({ "dir": directory, "rule": "exact", "name": name })
            }
            DirRule::VisibleLeaf => serde_json::json!({ "dir": directory, "rule": "visible_leaf" }),
            DirRule::SvgLeaf { max_leaf_bytes } => serde_json::json!({
                "dir": directory,
                "rule": "svg_leaf",
                "maxLeafBytes": max_leaf_bytes,
            }),
            DirRule::ScreensPackage => serde_json::json!({
                "dir": directory,
                "rule": "screens_package",
                "declaration": crate::issue::app_screen_pkg::SCREEN_MANIFEST,
                "leaf": "tag_stem.(js|css|svg|json)",
            }),
            DirRule::SkillsPackage => serde_json::json!({
                "dir": directory,
                "rule": "skills_package",
                "declaration": SKILL_MANIFEST,
                "entry": SKILL_ENTRY,
                "member": "<stem>.(md|txt|json|yaml|yml|csv|css|svg) | <scripts|references>/<stem>",
            }),
        })
        .collect();
    let document = serde_json::json!({
        "version": GRAMMAR_VERSION,
        "rootFiles": ROOT_MEMBERS,
        "segments": {
            "asciiOnly": true,
            "nonEmpty": true,
            "forbidExact": [".", ".."],
            "forbidLeadingDot": true,
            "forbidBackslash": true,
            "forbidNul": true,
        },
        "definitions": {
            "validTag": {
                "wholeInput": true,
                "minBytes": 1,
                "maxBytes": VALID_TAG_MAX_BYTES,
                "charset": VALID_TAG_CHARSET,
                "forbidLeading": "-",
            },
            "workflowMdTag": {
                "segment": true,
                "suffix": WORKFLOW_SUFFIX,
                "stem": "validTag of the leaf with every trailing suffix removed",
            },
            "visibleLeaf": { "segment": true },
            "svgLeaf": {
                "segment": true,
                "suffix": ASSET_SUFFIX,
                "maxLeafBytes": ASSET_MAX_LEAF_BYTES,
            },
            "screensPackage": {
                "path": "screens/<tag>/<leaf>",
                "tag": "validTag",
                "declaration": crate::issue::app_screen_pkg::SCREEN_MANIFEST,
                "assetStem": "validTag",
                "extensions": SCREEN_ASSET_EXTS,
            },
            // CAD-1867: a skill package. `files` in its manifest is the
            // declared inventory and must equal the present set exactly,
            // in both directions; `skill.json` is never listed in itself.
            "skillsPackage": {
                "path": "skills/<tag>/<member>",
                "tag": "validTag",
                "manifest": SKILL_MANIFEST,
                "entry": SKILL_ENTRY,
                "rootExtensions": SKILL_ROOT_EXTS,
                "nested": { "scripts": SKILL_SCRIPTS_EXTS, "references": SKILL_REFERENCES_EXTS },
                "maxDepth": 2,
                "dotfiles": "refused",
                "symlinks": "refused",
                "declaredEqualsPresent": true,
            },
            "exact": { "leaf": "the name carried by the directory entry" },
        },
        "directories": directories,
        "caps": {
            "fileBytes": MAX_FILE_BYTES,
            "screenAssetBytes": MAX_SCREEN_ASSET_BYTES,
            "bundleBytes": MAX_APP_BYTES,
            "files": MAX_FILES,
            "skillManifestBytes": SKILL_MANIFEST_BYTES,
            "skillFileBytes": SKILL_FILE_BYTES,
            "skillPackageBytes": SKILL_PACKAGE_BYTES,
            "skillFiles": SKILL_FILES_MAX,
        },
    });
    serde_json::to_string_pretty(&document).expect("grammar document serializes") + "\n"
}

#[cfg(test)]
mod bundle_grammar_tests {
    use super::*;

    #[test]
    fn admits_every_member_the_daemon_accepts() {
        for path in [
            "app.md",
            "app-chat.json",
            "app-assistant.json",
            "app-actions.json",
            "workflows/post.md",
            "views/app-views-v1.json",
            "rubrics/brand.md",
            "templates/base.liquid",
            "assets/crm.svg",
            "screens/crm/screens.json",
            "screens/crm/client.js",
            "screens/crm/styles.css",
            "skills/meeting-writer/skill.json",
            "skills/meeting-writer/SKILL.md",
            "skills/meeting-writer/notes.txt",
            "skills/meeting-writer/scripts/build.py",
            "skills/meeting-writer/references/style.md",
        ] {
            assert!(bundle_path_ok(path), "{path}");
        }
        // The descriptor the board transport used to refuse.
        assert!(root_member(crate::issue::app_action::FILE));
    }

    #[test]
    fn refuses_members_outside_the_grammar() {
        for path in [
            "",
            "readme.md",
            "app-actions/actions.json",
            "assets/crm.png",
            "assets/nested/icon.svg",
            "workflows/UPPER.md",
            "views/other.json",
            "screens/crm/deep/file.js",
            "skills/meeting-writer",
            "skills/meeting-writer/scripts",
            "skills/meeting-writer/scripts/deep/build.py",
            "skills/meeting-writer/scripts/build.rb",
            "skills/meeting-writer/references/build.py",
            "skills/meeting-writer/scripts/build.md",
            "skills/meeting-writer/.hidden.md",
            "skills/UPPER/SKILL.md",
            "../secret",
            "app.md/inner",
            "./app-actions.json",
        ] {
            assert!(!bundle_path_ok(path), "{path}");
        }
        // The snapshot and catalogue walkers cap an asset leaf at 128 bytes.
        assert!(bundle_path_ok(&format!("assets/{}.svg", "a".repeat(124))));
        assert!(!bundle_path_ok(&format!("assets/{}.svg", "a".repeat(125))));
    }

    /// The live artifact. Parity is checked against the current version only:
    /// the v1 and v2 documents are historical and stay untouched, so a v1/v2
    /// reader keeps the shape it was published with.
    fn grammar_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "contracts/bundle-grammar/v{GRAMMAR_VERSION}/grammar.json"
        ))
    }

    /// The published document is generated, never hand-written: this test is
    /// the only thing keeping `contracts/bundle-grammar/v3/grammar.json` equal
    /// to the predicate consumers are expected to apply.
    #[test]
    fn published_artifact_matches_the_predicate() {
        let path = grammar_path();
        let published = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        assert_eq!(
            published,
            bundle_grammar_json(),
            "regenerate with `cargo test --lib regenerate_bundle_grammar -- --ignored`"
        );
    }

    /// Regenerates the artifact. Ignored so an ordinary run can never bless
    /// drifted bytes by rewriting them before comparing.
    #[test]
    #[ignore = "writes the checked-in grammar artifact"]
    fn regenerate_bundle_grammar() {
        let path = grammar_path();
        std::fs::create_dir_all(path.parent().expect("grammar dir")).expect("create grammar dir");
        std::fs::write(&path, bundle_grammar_json()).expect("write grammar");
    }

    /// The document's normative claims must match the functions that enforce
    /// them, not just the table it was serialized from.
    #[test]
    fn published_definitions_match_the_predicate() {
        for byte in VALID_TAG_CHARSET.bytes() {
            let tag = format!("a{}", byte as char);
            assert!(crate::issue::model::valid_tag(&tag), "{tag}");
        }
        assert!(crate::issue::model::valid_tag(
            &"a".repeat(VALID_TAG_MAX_BYTES)
        ));
        // A trailing line terminator is the case a `$`-anchored regex would
        // wrongly admit in JavaScript; the host refuses it.
        for tag in ["", "-a", "A", "a_b", "a.b", "a ", "a\n"] {
            assert!(!crate::issue::model::valid_tag(tag), "{tag}");
        }
        assert!(!crate::issue::model::valid_tag(
            &"a".repeat(VALID_TAG_MAX_BYTES + 1)
        ));
        for ext in SCREEN_ASSET_EXTS {
            assert!(
                crate::issue::app_screen_pkg::leaf_ok(&format!("x.{ext}")),
                "{ext}"
            );
        }
        assert!(!crate::issue::app_screen_pkg::leaf_ok("x.txt"));
        // The workflow rule strips every trailing suffix, as the document says.
        assert!(bundle_path_ok("workflows/post.md.md"));
        // The asset rule is bounded by the published leaf cap.
        assert!(bundle_path_ok(&format!("assets/{}.svg", "a".repeat(124))));
        assert!(!bundle_path_ok(&format!("assets/{}.svg", "a".repeat(125))));
    }

    /// CAD-1848: `storage/` holds one flat `<validTag>.json` migration per
    /// file, and `app-storage.json` is its root declaration. The tag rule is
    /// the manifest tag rule (`model::valid_tag`): 1..=32 lowercase ASCII
    /// letters, digits or hyphens, never leading with a hyphen.
    #[test]
    fn admits_app_storage_root_and_flat_migration_members() {
        assert!(root_member("app-storage.json"));
        for path in [
            "app-storage.json",
            "storage/a.json",
            "storage/001-init.json",
            "storage/a-b0.json",
        ] {
            assert!(bundle_path_ok(path), "{path}");
        }
        // The bound is the published 32 bytes, and a trailing hyphen is
        // still a tag.
        assert!(bundle_path_ok(&format!("storage/{}.json", "a".repeat(32))));
        assert!(bundle_path_ok("storage/a-.json"));
    }

    /// Everything the flat shape does not admit refuses: traversal, empty
    /// segments, nesting, a second suffix, an unsupported or uppercase
    /// extension, and any stem the tag rule rejects.
    #[test]
    fn refuses_storage_members_outside_the_flat_tag_json_shape() {
        for path in [
            "",
            "storage",
            "storage/",
            "storage//a.json",
            "storage/../a.json",
            "storage/a/../b.json",
            "storage/nested/a.json",
            // Exactly one `.json` suffix.
            "storage/a",
            "storage/a.txt",
            "storage/a.JSON",
            "storage/a.json.json",
            // One tag-shaped stem.
            "storage/.json",
            "storage/.a.json",
            "storage/-a.json",
            "storage/a_b.json",
            "storage/a.b.json",
            "storage/a b.json",
            "storage/aß.json",
            // The segment rules hold inside `storage/` too.
            "storage\\a.json",
            "storage/a.json\0b",
        ] {
            assert!(!bundle_path_ok(path), "{path}");
        }
        // One byte past the published tag cap refuses.
        assert!(!bundle_path_ok(&format!("storage/{}.json", "a".repeat(33))));
    }

    /// The workflow half of the guard bundle: the CAD-1254 acceptance
    /// fixture's plan template, which the real `app::validate` gate already
    /// accepts (an unregistered `agent:` is a note at install, never an
    /// error).
    const STORAGE_GUARD_WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n---\n\n\
Why.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n";

    /// Lay a genuinely valid workflow app out at `root`. With `majors`, the
    /// manifest also declares
    /// `needs.requires.contracts: {app-storage: [<majors>]}`.
    fn write_workflow_app(root: &Path, majors: Option<&str>) {
        std::fs::create_dir_all(root.join("workflows")).unwrap();
        let requires = match majors {
            Some(majors) => format!("  requires: {{contracts: {{app-storage: [{majors}]}}}}\n"),
            None => String::new(),
        };
        std::fs::write(
            root.join(MANIFEST),
            format!(
                "---\napp: storage-guard\ntitle: Storage guard\nversion: '1'\n\
                 needs:\n  connections: []\n{requires}---\n\nGuide.\n"
            ),
        )
        .unwrap();
        std::fs::write(root.join("workflows/do.md"), STORAGE_GUARD_WORKFLOW).unwrap();
    }

    fn write_member(root: &Path, rel: &str, text: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("member parent")).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Real source validation over `root`, with the refusal text or `()`.
    fn validate_source(root: &Path) -> std::result::Result<(), String> {
        match validate(root, &HashSet::new(), &[]) {
            Ok(_) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// The fixture the storage guards start from is a valid package on its
    /// own: a refusal below is storage's, never a broken baseline.
    #[test]
    fn the_storage_guard_baseline_is_a_valid_app() {
        let root = tempfile::tempdir().unwrap();
        write_workflow_app(root.path(), None);
        assert_eq!(validate_source(root.path()), Ok(()));
    }

    /// Storage members are admitted only for a package that declares the
    /// exact host contract it needs: `needs.requires.contracts.app-storage:
    /// [1]`. Absent, a different major, or an extra major all refuse.
    #[test]
    fn storage_members_refuse_without_the_exact_app_storage_declaration() {
        let member_only: &[&str] = &["storage/a.json"];
        let root_only: &[&str] = &["app-storage.json"];
        let root_and_member: &[&str] = &["app-storage.json", "storage/a.json"];
        for (label, majors, files) in [
            ("member only", None, member_only),
            ("root only", None, root_only),
            ("root and member", None, root_and_member),
            ("major 2", Some("2"), root_and_member),
            ("majors 1 and 2", Some("1, 2"), root_and_member),
        ] {
            let root = tempfile::tempdir().unwrap();
            write_workflow_app(root.path(), majors);
            for &rel in files {
                write_member(root.path(), rel, "{}");
            }
            let error = validate_source(root.path())
                .expect_err(&format!("{label}: storage must refuse without [1]"));
            assert!(
                error.contains(
                    "app storage members require needs.requires.contracts.app-storage: [1]"
                ),
                "{label}: {error}"
            );
        }
    }

    /// A declaration without its root member refuses — the pairing runs
    /// both ways, so `requires` alone cannot promise storage.
    #[test]
    fn storage_declaration_without_its_root_member_refuses() {
        let no_members: &[&str] = &[];
        let member_only: &[&str] = &["storage/a.json"];
        for files in [no_members, member_only] {
            let root = tempfile::tempdir().unwrap();
            write_workflow_app(root.path(), Some("1"));
            for &rel in files {
                write_member(root.path(), rel, "{}");
            }
            let error = validate_source(root.path())
                .expect_err("a declared app-storage still needs app-storage.json");
            assert!(
                error.contains("app-storage requires app-storage.json"),
                "{error}"
            );
        }
    }

    /// Recognizing inert members is not a native storage capability: the
    /// exact pairing is admitted by the bundle checks, and the package still
    /// refuses for the runtime this host does not have — never a fabricated
    /// native success.
    #[test]
    fn declared_storage_fails_for_the_missing_native_runtime() {
        let root = tempfile::tempdir().unwrap();
        write_workflow_app(root.path(), Some("1"));
        write_member(root.path(), "app-storage.json", "{}");
        write_member(root.path(), "storage/001-init.json", "{}");
        let error = validate_source(root.path()).expect_err("no native app-storage runtime");
        assert!(error.contains("app is not installable"), "{error}");
        assert!(
            error.contains("app requires host contract 'app-storage' major 1"),
            "{error}"
        );
        assert!(
            error.contains("this host supports none (unknown contract)"),
            "{error}"
        );
        // The refusal is the missing runtime, not the bundle pairing.
        assert!(
            !error.contains("app storage members require"),
            "the exact [1] declaration pairs with the members: {error}"
        );
        assert!(!error.contains("requires app-storage.json"), "{error}");
    }

    /// Source validation refuses a storage member that is a link or sits in
    /// a nested directory before any bytes are read.
    #[test]
    fn storage_source_refuses_links_and_nested_directories() {
        let linked = tempfile::tempdir().unwrap();
        write_workflow_app(linked.path(), Some("1"));
        write_member(linked.path(), "app-storage.json", "{}");
        write_member(linked.path(), "storage/a.json", "{}");
        std::os::unix::fs::symlink(
            linked.path().join("storage/a.json"),
            linked.path().join("storage/b.json"),
        )
        .unwrap();
        let error = validate_source(linked.path()).expect_err("a link is not app content");
        assert!(error.contains("storage/b.json"), "{error}");
        assert!(error.contains("symlink"), "{error}");

        let nested = tempfile::tempdir().unwrap();
        write_workflow_app(nested.path(), Some("1"));
        write_member(nested.path(), "app-storage.json", "{}");
        write_member(nested.path(), "storage/nested/a.json", "{}");
        let error = validate_source(nested.path()).expect_err("storage/ is flat");
        assert!(error.contains("storage/nested"), "{error}");
    }
}

/// Most files a bundle may carry.
const MAX_FILES: usize = 128;

/// Largest bundle, all files together — an A1 app is text.
pub(crate) const MAX_APP_BYTES: u64 = 2 * 1024 * 1024;

/// The `app.md` definition: the frontmatter fields plus the body, the
/// guide agents read. `needs.connections` declares the slots the app's
/// workflows may name in their `uses:` lines; `summary` is the optional
/// one-line purpose the board shows people (wording — the guide's bytes
/// are what the digest covers, so adding one never re-gates an app).
#[derive(Clone, Debug)]
pub struct Manifest {
    pub app: String,
    pub title: String,
    pub version: String,
    pub connections: Vec<String>,
    pub capabilities: BTreeMap<String, CapabilityNeed>,
    pub summary: Option<String>,
    /// CAD-864: `needs.views.contract` — the one descriptor contract
    /// this bundle's `views/app-views-v1.json` declares. `Some` only
    /// when the manifest declared it; the file and declaration pair up
    /// at `validate`/`validate_texts` (either alone refuses).
    pub view_contract: Option<String>,
    /// Host requirements declared under `needs.requires`. Absent means
    /// a legacy package; malformed declarations fail manifest parsing.
    pub requires: Compat,
    /// CAD-1129 §3: the display-only catalog block (`listing:`), plain
    /// JSON already host-validated — it can declare no capability.
    pub listing: Option<crate::issue::app_listing::Listing>,
    pub guide: String,
}

impl Manifest {
    /// The installation's reserved `file.upload` declaration, if any
    /// (CAD-1168/CAD-1114 subset). A declaration is not a grant: the
    /// chat upload service re-proves the current exact-digest consent and
    /// this declaration at every upload, reference and read, never from a
    /// browser projection.
    pub fn file_upload(&self) -> Option<&CapabilityNeed> {
        self.capabilities
            .values()
            .find(|need| need.is_file_upload())
    }
}

/// The app names the result it needs. Reviewed providers own exact tools,
/// scopes and preparation; none of those are selected by bundle bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityNeed {
    pub schema: u32,
    pub capability: String,
    pub version: u32,
    pub action: String,
    pub resource_kind: String,
    pub effect: String,
}
impl CapabilityNeed {
    /// The reserved `file.upload` capability name (CAD-1168/CAD-1114
    /// subset): the host may stage a source for that approved
    /// installation's chat. It grants no provider connection, import,
    /// release, publish, send or worker dispatch action, and host custody
    /// is local draft material, not external-send authority. There is
    /// exactly one accepted tuple for this name — see `is_file_upload`.
    pub const FILE_UPLOAD: &'static str = "file.upload";

    /// Exactly the reserved `file.upload` declaration: schema 1, version 1,
    /// action `attach`, resource_kind `installation`, effect `draft`. The
    /// name alone is not the declaration — a variant tuple of the same name
    /// is refused by `validate` and is not this predicate. Later runtime
    /// reference checks must re-prove this declaration against the exact
    /// consented digest; this predicate is not itself an authority.
    pub fn is_file_upload(&self) -> bool {
        self.schema == 1
            && self.capability == Self::FILE_UPLOAD
            && self.version == 1
            && self.action == "attach"
            && self.resource_kind == "installation"
            && self.effect == "draft"
    }

    pub fn validate(&self) -> Result<()> {
        // The reserved `file.upload` name is exact-tuple-only: it never
        // falls through to the generic read/draft `connection_account`
        // broker slot, so a wrong effect/resource/action/schema/version
        // refuses instead of being accepted as a different capability.
        let file_upload = self.is_file_upload();
        let publication = self.capability == "text.publish"
            && self.version == 1
            && self.action == "publish"
            && self.effect == "send"
            && self.resource_kind == "connection_account";
        let bounded_read_or_draft = self.capability != "text.publish"
            && self.capability != Self::FILE_UPLOAD
            && matches!(self.effect.as_str(), "read" | "draft")
            && self.version == 1
            && self.capability.len() <= 64
            && self.capability.split('.').count() == 2
            && self.capability.split('.').all(model::valid_tag)
            && !self.action.is_empty()
            && self.action.len() <= 32
            && self
                .action
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            && self.resource_kind == "connection_account";
        if self.schema != 1 || !(file_upload || publication || bounded_read_or_draft) {
            return Err(Error::rejected("unsupported app capability contract"));
        }
        Ok(())
    }
}

/// `apps/<name>.yaml` — the install record beside the content folder:
/// where the bundle came from (a git install pins the commit SHA), when
/// and by whom, and each slot's binding. In `bindings`, `null` is an
/// explicit unbind and an absent slot is the `local` default. `team`
/// (CAD-577) is the app's default team — one agent alias per workflow
/// input role — an operator-only write that is NOT part of the gate
/// digest (a team change never re-requires approval). `install_id`
/// (CAD-577) is minted fresh on every install and kept across `app
/// update`. Approvals and derived grants bind to it: a missing id, or
/// one from a previous install of the same bytes, is not a match.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: u32,
    pub app: String,
    /// This install's id. Empty on a record written before install ids
    /// existed — that never matches an approval, so the operator
    /// re-approves once.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub install_id: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, Option<String>>,
    /// The app's default team (CAD-577): input role name → agent alias.
    /// Stored with the install record, never in the digest.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub team: BTreeMap<String, String>,
    pub installed_at: String,
    pub installed_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// CAD-1129 H5: the epoch this install was soft-removed. A set
    /// value hides it from Open, the home and `app_workspace_list`,
    /// and refuses run creation. Restore clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<i64>,
    /// The last epoch a restore is admitted; after it, `restore`
    /// refuses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_after: Option<i64>,
    /// The epoch the soft-removed bytes may be purged — kept for
    /// CAD-1130's flag-gated purge; `remove` never writes it before
    /// then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_after: Option<i64>,
    /// Catalogue release this installation was checked against. Absence
    /// is a path, Git or other non-catalogue install — never a guessed
    /// label. The locator is provenance, not a grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalogue: Option<CatalogueOrigin>,
}

/// A published catalogue release named by id, version and digest.
/// Matching bytes do not make the caller trusted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueOrigin {
    pub id: String,
    pub version: String,
    pub digest: String,
}

/// Where an installed bundle came from. `git` records the exact commit
/// the clone pinned — a branch or tag name is never trusted later.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    Path {
        path: String,
    },
    Git {
        url: String,
        sha: String,
        /// CAD-1129: the bundle subdirectory inside the repo, when the
        /// install came through the Explorer's Git check.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dir: Option<String>,
    },
    /// CAD-1129: installed from the host's embedded built-in catalog —
    /// the catalog entry id and the bundle digest it shipped.
    Builtin {
        id: String,
        digest: String,
    },
}

impl Record {
    /// The connection a slot runs against: the recorded binding, an
    /// explicit unbind (`None`), or the `local` default.
    pub fn binding(&self, slot: &str) -> Option<&str> {
        match self.bindings.get(slot) {
            Some(Some(conn)) => Some(conn),
            Some(None) => None,
            None => Some(LOCAL_CONNECTION),
        }
    }
}

/// An app or workflow name — tag-shaped, like the workflow names beside
/// it (`apps/<app>/workflows/<wf>.md` addresses a run as `<app>/<wf>`).
fn check_name(name: &str, what: &str) -> Result<()> {
    if !model::valid_tag(name) {
        return Err(Error::rejected(format!(
            "{what} '{name}' — 1-32 lowercase letters, digits or hyphens"
        )));
    }
    Ok(())
}

/// `<pm>/<project>/apps/` — `project` is a key, never a path fragment,
/// and a symlinked `apps/` is refused rather than followed.
fn dir_of(pm_dir: &Path, project: &str) -> Result<PathBuf> {
    model::check_key(project)?;
    let dir = pm_dir.join(project).join(DIR);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{project}/{DIR}/ is a symlink — the tracker never follows links"
        )));
    }
    Ok(dir)
}

/// `apps/<name>/` — the content folder; must exist as a real directory.
fn app_dir(pm_dir: &Path, project: &str, name: &str) -> Result<PathBuf> {
    check_name(name, "app name")?;
    let dir = dir_of(pm_dir, project)?.join(name);
    match dir.symlink_metadata() {
        Ok(m) if m.is_symlink() => Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            dir.display()
        ))),
        Ok(m) if m.is_dir() => Ok(dir),
        _ => Err(Error::rejected(format!(
            "no app '{name}' installed in {project} — `cadence app ls \
             --project {project}` lists them"
        ))),
    }
}

/// Whether `apps/<name>/` is a real directory. The grant sweep uses
/// this to tell a removed app from a digest that failed to read.
pub fn is_installed(pm_dir: &Path, project: &str, name: &str) -> bool {
    app_dir(pm_dir, project, name).is_ok()
}

/// `apps/<name>.yaml` — the install record.
fn record_file(pm_dir: &Path, project: &str, name: &str) -> Result<PathBuf> {
    check_name(name, "app name")?;
    Ok(dir_of(pm_dir, project)?.join(format!("{name}.yaml")))
}

/// Read the install record: real file only, schema-checked, and the
/// record must name the folder it claims.
pub fn read_record(pm_dir: &Path, project: &str, name: &str) -> Result<Record> {
    let file = record_file(pm_dir, project, name)?;
    if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            file.display()
        )));
    }
    let text = std::fs::read_to_string(&file).map_err(|_| {
        Error::rejected(format!(
            "app '{name}' in {project} has no install record {} — it was not \
             installed by `cadence app install` (or was removed by hand)",
            file.display()
        ))
    })?;
    let record: Record = serde_yaml::from_str(&text).map_err(|e| {
        Error::rejected(format!(
            "{} is not a valid install record: {e}",
            file.display()
        ))
    })?;
    if record.schema != 1 {
        return Err(Error::rejected(format!(
            "{}: schema {} — this cadence reads schema 1",
            file.display(),
            record.schema
        )));
    }
    if record.app != name {
        return Err(Error::rejected(format!(
            "{} names app '{}' — the file is for '{name}'",
            file.display(),
            record.app
        )));
    }
    Ok(record)
}

/// Write the install record (deterministic YAML, 0644).
fn write_record(file: &Path, record: &Record) -> Result<()> {
    let text = serde_yaml::to_string(record)
        .map_err(|e| Error::internal(format!("install record: {e}")))?;
    write_verified(file, &text)?;
    Ok(())
}

/// The `app.md` contract: frontmatter is a mapping of
/// [`MANIFEST_KEYS`] — `app` (the folder/install name, tag-shaped),
/// `title`, `version`, and `needs.connections` a distinct list of
/// tag-shaped slots. Later-stage keys refuse with their stage named.
pub fn parse_manifest(text: &str) -> Result<Manifest> {
    let (yaml, body) = parse::split_front(text).map_err(|e| {
        Error::rejected(format!(
            "{e} — app.md needs frontmatter: app, title, version, needs"
        ))
    })?;
    let meta: serde_yaml::Value = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("app.md frontmatter: {e}")))?;
    let serde_yaml::Value::Mapping(map) = meta else {
        return Err(Error::rejected(
            "app.md frontmatter is not a mapping — app, title, version, needs",
        ));
    };
    for key in map.keys() {
        let k = key.as_str().unwrap_or_default();
        if GATED_KEYS.contains(&k) {
            return Err(Error::rejected(format!(
                "app.md frontmatter key '{k}' is a later stage (A2/A3) — v0 installs \
                 app.md + workflows/ + optional rubrics/, templates/, views/ only"
            )));
        }
        if !MANIFEST_KEYS.contains(&k) {
            return Err(Error::rejected(format!(
                "app.md frontmatter key '{k}' is unknown — v0 knows {}; anything else \
                 can never be installed",
                MANIFEST_KEYS.join(", ")
            )));
        }
    }
    // Decision 8: `cost`/`price`/`pricing` are refused anywhere in the
    // frontmatter, at any depth — the UI never shows a price.
    crate::issue::app_listing::refuse_price_keys(&serde_yaml::Value::Mapping(map.clone()))?;
    let get = |k: &str| map.get(serde_yaml::Value::String(k.to_string()));
    let need_str = |k: &str, what: &str| -> Result<String> {
        match get(k).and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
            _ => Err(Error::rejected(format!("app.md needs `{k}:` — {what}"))),
        }
    };
    let app = need_str("app", "the app name — the folder it installs as")?;
    check_name(&app, "app name")?;
    // `/apps/explore` is the catalog route — an app named `explore`
    // would shadow it on the board (CAD-1129 H10).
    if app == "explore" {
        return Err(Error::rejected(
            "app name 'explore' is reserved for the app catalog route",
        ));
    }
    let title = need_str("title", "one line naming the app")?;
    let version = need_str("version", "a version string like 0.1.0")?;
    for (k, v, cap) in [("title", &title, 120), ("version", &version, 40)] {
        if v.chars().count() > cap || v.chars().any(char::is_control) {
            return Err(Error::rejected(format!(
                "app.md `{k}:` — ≤{cap} chars, no control characters"
            )));
        }
    }
    // `summary:` is optional wording — the board's one-line purpose.
    let summary = match get("summary") {
        None | Some(serde_yaml::Value::Null) => None,
        Some(serde_yaml::Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else if s.chars().count() > 160 || s.chars().any(char::is_control) {
                return Err(Error::rejected(
                    "app.md `summary:` — ≤160 chars, no control characters",
                ));
            } else {
                Some(s.to_string())
            }
        }
        Some(_) => {
            return Err(Error::rejected(
                "app.md `summary:` is one line of text — what the app is for",
            ))
        }
    };
    let mut connections = Vec::new();
    let mut capabilities = BTreeMap::new();
    let mut view_contract: Option<String> = None;
    if let Some(needs) = get("needs") {
        let serde_yaml::Value::Mapping(needs) = needs else {
            return Err(Error::rejected(
                "app.md `needs:` is a mapping — v0 knows `needs.connections`",
            ));
        };
        for key in needs.keys() {
            let k = key.as_str().unwrap_or_default();
            if !matches!(k, "connections" | "capabilities" | "views" | "requires") {
                return Err(Error::rejected(format!(
                    "app.md `needs.{k}` is unknown — v0 knows `needs.connections`, \
                     `needs.capabilities`, `needs.views`, `needs.requires`"
                )));
            }
        }
        if let Some(value) = needs.get(serde_yaml::Value::String("views".into())) {
            let serde_yaml::Value::Mapping(views) = value else {
                return Err(Error::rejected(
                    "app.md `needs.views` is a mapping — `contract: app-views/v1`",
                ));
            };
            for key in views.keys() {
                if key.as_str() != Some("contract") {
                    return Err(Error::rejected(
                        "app.md `needs.views` knows only `contract`",
                    ));
                }
            }
            match views.get(serde_yaml::Value::String("contract".into())) {
                Some(serde_yaml::Value::String(c)) if c == app_view::CONTRACT => {
                    view_contract = Some(app_view::CONTRACT.to_string());
                }
                _ => {
                    return Err(Error::rejected(
                        "app.md `needs.views.contract` is exactly `app-views/v1` — \
                         the contract the bundle's `views/app-views-v1.json` declares",
                    ));
                }
            }
        }
        if let Some(value) = needs.get(serde_yaml::Value::String("capabilities".into())) {
            capabilities =
                serde_yaml::from_value::<BTreeMap<String, CapabilityNeed>>(value.clone())
                    .map_err(|e| Error::rejected(format!("app.md needs.capabilities: {e}")))?;
            if capabilities.len() > 8 {
                return Err(Error::rejected(
                    "an app supports at most eight capability slots",
                ));
            }
            if capabilities
                .values()
                .filter(|need| need.effect == "send")
                .count()
                > 1
            {
                return Err(Error::rejected(
                    "an app supports at most one publication capability slot",
                ));
            }
            for (slot, declaration) in &capabilities {
                check_name(slot, "publication slot")?;
                declaration.validate()?;
            }
        }
        if let Some(list) = needs.get(serde_yaml::Value::String("connections".to_string())) {
            let serde_yaml::Value::Sequence(list) = list else {
                return Err(Error::rejected(
                    "app.md `needs.connections` is a list of slot names — [publish, cms]",
                ));
            };
            for item in list {
                let Some(slot) = item.as_str() else {
                    return Err(Error::rejected(
                        "app.md `needs.connections` entries are slot names — [publish, cms]",
                    ));
                };
                check_name(slot, "connection slot")?;
                if connections.iter().any(|s| s == slot) {
                    return Err(Error::rejected(format!(
                        "connection slot '{slot}' is declared twice"
                    )));
                }
                connections.push(slot.to_string());
            }
        }
    }
    if capabilities.keys().any(|slot| connections.contains(slot)) {
        return Err(Error::rejected(
            "a publication slot cannot also be a legacy connection slot",
        ));
    }
    // `listing:` is display-only catalog copy (CAD-1129 §3). Slots it
    // names must be declared; the block itself grants nothing. Asset
    // references are collected on the `Listing` and `validate_contents`
    // proves each one is in the bundle's own inventory.
    let listing = match get("listing") {
        None | Some(serde_yaml::Value::Null) => None,
        Some(value) => {
            let yaml_len = serde_yaml::to_string(value)
                .map_err(|e| Error::rejected(format!("app.md listing: {e}")))?
                .len();
            let mut slots: Vec<String> = connections.clone();
            slots.extend(capabilities.keys().cloned());
            Some(crate::issue::app_listing::parse(value, yaml_len, &slots)?)
        }
    };
    let requires =
        app_compat::parse_requires(get("needs").and_then(serde_yaml::Value::as_mapping))?;
    Ok(Manifest {
        app,
        title,
        version,
        connections,
        capabilities,
        summary,
        view_contract,
        requires,
        listing,
        guide: body.to_string(),
    })
}

/// The verified file list of an app folder: `rel-path → source path`.
/// The folder is never followed past a symlink — every entry is checked
/// lstat-style — and only the known top-level entries are read at all.
/// `workflows/` holds flat `*.md` files (the stem a tag-shaped name);
/// `rubrics/` and `templates/` are flat text dirs. Everything else —
/// a nested dir, a fifo/socket/device, a dotfile, a name that is not
/// UTF-8, an oversize file — refuses.
fn bundle_files(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let meta = root
        .symlink_metadata()
        .map_err(|e| Error::rejected(format!("cannot stat app source {}: {e}", root.display())))?;
    if meta.is_symlink() {
        return Err(Error::rejected(format!(
            "app source {} is a symlink — an app is a real folder",
            root.display()
        )));
    }
    if !meta.is_dir() {
        return Err(Error::rejected(format!(
            "app source {} is not a folder",
            root.display()
        )));
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut manifest = false;
    let entry_err = |name: &str, why: &str| -> Error {
        Error::rejected(format!("app source entry '{name}': {why}"))
    };
    for entry in std::fs::read_dir(root)?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(Error::rejected(
                "app source carries a file name that is not UTF-8",
            ));
        };
        if name.starts_with('.') {
            // A clone's `.git` is never app content; other dotfiles are
            // refused outright — nothing in an app is hidden.
            if name == ".git" {
                continue;
            }
            return Err(entry_err(name, "dotfiles are not app content"));
        }
        let ft = entry
            .file_type()
            .map_err(|e| Error::rejected(format!("cannot stat app source entry '{name}': {e}")))?;
        if ft.is_symlink() {
            return Err(entry_err(
                name,
                "a symlink — an app folder holds real files only",
            ));
        }
        if name == MANIFEST {
            if !ft.is_file() {
                return Err(entry_err(name, "app.md is a regular file"));
            }
            manifest = true;
            files.push((name.to_string(), entry.path()));
            continue;
        }
        // CAD-1110/CAD-1415: the data-only descriptors sit beside app.md —
        // one list with the source loader and the upload transport.
        if root_member(name) {
            if !ft.is_file() {
                return Err(entry_err(name, "app-chat.json is a regular file"));
            }
            files.push((name.to_string(), entry.path()));
            continue;
        }
        if TOP_DIRS.contains(&name) {
            if !ft.is_dir() {
                return Err(entry_err(name, "expected a directory"));
            }
            dirs.push((name.to_string(), entry.path()));
            continue;
        }
        return Err(Error::rejected(format!(
            "app source entry '{name}' — v0 knows app.md, app-chat.json, workflows/, \
             rubrics/, templates/, screens/, skills/, views/; everything else refuses"
        )));
    }
    if !manifest {
        return Err(Error::rejected(format!(
            "app source {} has no app.md — frontmatter `app`, `title`, `version` \
             and the agent guide are required",
            root.display()
        )));
    }
    // CAD-1177: an app bundles workflows and/or screens — a standalone
    // tools plugin ships only `screens/` (the v2 tool declarations are
    // its unit of work, checked later in `validate_texts`); a
    // workflow-driven app ships `workflows/`. One of the two must be
    // present or the bundle installs nothing usable.
    if !dirs.iter().any(|(d, _)| d == "workflows" || d == "screens") {
        return Err(Error::rejected(
            "an app needs workflows/ or screens/ — an app with neither installs \
             nothing runnable",
        ));
    }
    for (top, dir) in &dirs {
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(Error::rejected(format!(
                    "{top}/ carries a file name that is not UTF-8"
                )));
            };
            if name.starts_with('.') {
                return Err(entry_err(
                    &format!("{top}/{name}"),
                    "dotfiles are not app content",
                ));
            }
            let ft = entry
                .file_type()
                .map_err(|e| Error::rejected(format!("cannot stat '{top}/{name}': {e}")))?;
            if ft.is_symlink() {
                return Err(entry_err(
                    &format!("{top}/{name}"),
                    "a symlink — an app folder holds real files only",
                ));
            }
            if *top == "screens" {
                // A screens member is a <tag>/ directory handled by the
                // nested pass below — never a flat file.
                if !ft.is_dir() {
                    return Err(entry_err(
                        &format!("{top}/{name}"),
                        "a screen package is a <tag>/ directory",
                    ));
                }
                continue;
            }
            if *top == "skills" {
                // CAD-1867: a skills member is a <tag>/ directory handled
                // by the nested pass below — never a flat file.
                if !ft.is_dir() {
                    return Err(entry_err(
                        &format!("{top}/{name}"),
                        "a skill package is a <tag>/ directory",
                    ));
                }
                continue;
            }
            if ft.is_dir() {
                return Err(entry_err(
                    &format!("{top}/{name}"),
                    "v0 app dirs are flat — no nested folders",
                ));
            }
            if !ft.is_file() {
                return Err(entry_err(&format!("{top}/{name}"), "not a regular file"));
            }
            if top == "workflows" {
                let Some(stem) = name.strip_suffix(".md") else {
                    return Err(entry_err(
                        &format!("{top}/{name}"),
                        "workflows are plan-template files ending in .md",
                    ));
                };
                check_name(stem, "workflow name")?;
            }
            if top == "views" && name != app_view::FILE {
                // The filename pins the contract version — a bundle
                // cannot rename the descriptor or ship a second one.
                return Err(entry_err(
                    &format!("{top}/{name}"),
                    "views/ holds exactly app-views-v1.json",
                ));
            }
            let relative = format!("{top}/{name}");
            if top == "storage" && !bundle_path_ok(&relative) {
                return Err(entry_err(
                    &relative,
                    "storage migrations are flat <tag>.json files",
                ));
            }
            files.push((relative, entry.path()));
        }
        // screens/<tag>/<leaf> — the one nested level v1 allows: each
        // `<tag>` is a tag-named subdir holding the declaration and flat
        // asset leaves. Anything deeper, a non-dir member, or a bad leaf
        // extension refuses.
        if top == "screens" {
            for tag_entry in std::fs::read_dir(dir)?.flatten() {
                let tag = tag_entry.file_name();
                let Some(tag) = tag.to_str() else {
                    return Err(Error::rejected("screens/ carries a non-UTF-8 name"));
                };
                if tag.starts_with('.') {
                    return Err(entry_err(
                        &format!("screens/{tag}"),
                        "dotfiles are not app content",
                    ));
                }
                let ft = tag_entry
                    .file_type()
                    .map_err(|e| Error::rejected(format!("cannot stat 'screens/{tag}': {e}")))?;
                if ft.is_symlink() {
                    return Err(entry_err(
                        &format!("screens/{tag}"),
                        "a symlink — an app folder holds real files only",
                    ));
                }
                if !ft.is_dir() {
                    return Err(entry_err(
                        &format!("screens/{tag}"),
                        "a screen package is a <tag>/ directory",
                    ));
                }
                check_name(tag, "screen tag")?;
                for leaf in std::fs::read_dir(tag_entry.path())?.flatten() {
                    let leaf_name = leaf.file_name();
                    let Some(leaf_name) = leaf_name.to_str() else {
                        return Err(Error::rejected(
                            "screens/<tag>/ carries a non-UTF-8 leaf name",
                        ));
                    };
                    if leaf_name.starts_with('.') {
                        return Err(entry_err(
                            &format!("screens/{tag}/{leaf_name}"),
                            "dotfiles are not app content",
                        ));
                    }
                    let lft = leaf.file_type().map_err(|e| {
                        Error::rejected(format!("cannot stat 'screens/{tag}/{leaf_name}': {e}"))
                    })?;
                    if lft.is_symlink() {
                        return Err(entry_err(
                            &format!("screens/{tag}/{leaf_name}"),
                            "a symlink — an app folder holds real files only",
                        ));
                    }
                    if !lft.is_file() {
                        return Err(entry_err(
                            &format!("screens/{tag}/{leaf_name}"),
                            "screen assets are flat files — no nested folders",
                        ));
                    }
                    let rel = format!("screens/{tag}/{leaf_name}");
                    if !crate::issue::app_screen_pkg::leaf_ok(leaf_name) {
                        return Err(entry_err(
                            &rel,
                            "a screen asset is <stem>.<js|css|svg|json>, or screens.json",
                        ));
                    }
                    files.push((rel, leaf.path()));
                }
            }
            // screens/ holds subdirs only — the flat-member loop above
            // collected none for it; drop the loop's own (empty) result.
        }
        // CAD-1867: skills/<tag>/<member>, where `<member>` is a flat root
        // text leaf or a file in `scripts/` or `references/`. Exactly the
        // grammar `skill_member_ok` states — a directory is only ever
        // `scripts/` or `references/`, never a file, and nothing nests
        // deeper.
        if top == "skills" {
            for tag_entry in std::fs::read_dir(dir)?.flatten() {
                let tag = tag_entry.file_name();
                let Some(tag) = tag.to_str() else {
                    return Err(Error::rejected("skills/ carries a non-UTF-8 name"));
                };
                if tag.starts_with('.') {
                    return Err(entry_err(
                        &format!("skills/{tag}"),
                        "dotfiles are not app content",
                    ));
                }
                let ft = tag_entry
                    .file_type()
                    .map_err(|e| Error::rejected(format!("cannot stat 'skills/{tag}': {e}")))?;
                if ft.is_symlink() {
                    return Err(entry_err(
                        &format!("skills/{tag}"),
                        "a symlink — an app folder holds real files only",
                    ));
                }
                if !ft.is_dir() {
                    return Err(entry_err(
                        &format!("skills/{tag}"),
                        "a skill package is a <tag>/ directory",
                    ));
                }
                check_name(tag, "skill name")?;
                let tagdir = tag_entry.path();
                for leaf in std::fs::read_dir(&tagdir)?.flatten() {
                    let leaf_name = leaf.file_name();
                    let Some(leaf_name) = leaf_name.to_str() else {
                        return Err(Error::rejected(
                            "skills/<tag>/ carries a non-UTF-8 leaf name",
                        ));
                    };
                    let rel = format!("skills/{tag}/{leaf_name}");
                    if leaf_name.starts_with('.') {
                        return Err(entry_err(&rel, "dotfiles are not app content"));
                    }
                    let lft = leaf
                        .file_type()
                        .map_err(|e| Error::rejected(format!("cannot stat '{rel}': {e}")))?;
                    if lft.is_symlink() {
                        return Err(entry_err(
                            &rel,
                            "a symlink — an app folder holds real files only",
                        ));
                    }
                    if lft.is_dir() {
                        if !skill_subdir_ok(leaf_name) {
                            return Err(entry_err(
                                &rel,
                                "a skill package nests only scripts/ or references/",
                            ));
                        }
                        for member in std::fs::read_dir(leaf.path())?.flatten() {
                            let member_name = member.file_name();
                            let Some(member_name) = member_name.to_str() else {
                                return Err(Error::rejected(format!(
                                    "{rel}/ carries a non-UTF-8 name"
                                )));
                            };
                            let member_rel = format!("{rel}/{member_name}");
                            if member_name.starts_with('.') {
                                return Err(entry_err(&member_rel, "dotfiles are not app content"));
                            }
                            let mft = member.file_type().map_err(|e| {
                                Error::rejected(format!("cannot stat '{member_rel}': {e}"))
                            })?;
                            if mft.is_symlink() {
                                return Err(entry_err(
                                    &member_rel,
                                    "a symlink — an app folder holds real files only",
                                ));
                            }
                            if !mft.is_file() {
                                return Err(entry_err(
                                    &member_rel,
                                    "skill members are flat files — no deeper folders",
                                ));
                            }
                            let package_member = format!("{leaf_name}/{member_name}");
                            if !skill_member_ok(&package_member) {
                                return Err(entry_err(
                                    &member_rel,
                                    &format!(
                                        "a {leaf_name}/ member is <stem>.<{}>",
                                        skill_nested_ext(leaf_name).unwrap_or(&[]).join("|")
                                    ),
                                ));
                            }
                            files.push((member_rel, member.path()));
                        }
                        continue;
                    }
                    if !lft.is_file() {
                        return Err(entry_err(&rel, "not a regular file"));
                    }
                    if !skill_member_ok(leaf_name) {
                        return Err(entry_err(
                            &rel,
                            &format!(
                                "a package root leaf is <stem>.<{}>",
                                SKILL_ROOT_EXTS.join("|")
                            ),
                        ));
                    }
                    files.push((rel, leaf.path()));
                }
            }
        }
    }
    if files.len() > MAX_FILES {
        return Err(Error::rejected(format!(
            "app carries {} files — at most {MAX_FILES}",
            files.len()
        )));
    }
    let mut total = 0u64;
    for (rel, path) in &files {
        let len = path.symlink_metadata().map(|m| m.len()).unwrap_or(0);
        if len > file_cap(rel) {
            return Err(Error::rejected(format!(
                "{rel} is {len} bytes — a file is at most {}",
                file_cap(rel)
            )));
        }
        total += len;
        if total > MAX_APP_BYTES {
            return Err(Error::rejected(format!(
                "app is over {MAX_APP_BYTES} bytes of content — split it"
            )));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// The `uses:` slots a workflow names: each `uses:` metadata value is a
/// static slot list — `[a, b]` or `a, b` — never a placeholder (the
/// slot contract must be checkable at install).
fn workflow_slots(text: &str) -> Result<Vec<String>> {
    let (_, body) = parse::split_front(text)?;
    let metas = workflow::ticket_meta(body)?;
    let mut slots = Vec::new();
    for meta in metas {
        for (key, value) in meta {
            if key != "uses" {
                continue;
            }
            if value.contains("{{") {
                return Err(Error::rejected(
                    "a `uses:` slot is static — never a `{{input}}` placeholder",
                ));
            }
            for tok in value
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                slots.push(tok.to_string());
            }
        }
    }
    Ok(slots)
}

/// Integrity-checked package bytes and observations, not execution authority.
/// Native validation additionally enforces the actual host requirements.
pub(crate) struct Validated {
    pub(crate) manifest: Manifest,
    pub(crate) files: Vec<(String, String)>,
    pub(crate) secret_warnings: Vec<crate::secret::Finding>,
    pub(crate) notes: Vec<String>,
    pub(crate) compatibility: Value,
}

/// Read a bundle's files as UTF-8 text and validate them: the manifest
/// parses, every workflow passes the CAD-487 `workflow check` (same
/// `check_text`), every `uses:` names a declared slot, and the secret
/// guard runs over each file — an install refuses on any error.
pub(crate) fn validate(
    root: &Path,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Validated> {
    let paths = bundle_files(root)?;
    let mut files = Vec::with_capacity(paths.len());
    let mut secret_warnings = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (rel, path) in paths {
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::rejected(format!("cannot read app file {rel}: {e}")))?;
        if bytes.len() as u64 > file_cap(&rel) {
            return Err(Error::rejected(format!(
                "{rel} is {} bytes — a file is at most {}",
                bytes.len(),
                file_cap(&rel)
            )));
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                errors.push(format!(
                    "{rel}: not UTF-8 text — an A1 app carries text only"
                ));
                continue;
            }
        };
        match crate::secret::guard(&format!("app file {rel}"), &text) {
            Ok(findings) => secret_warnings.extend(findings),
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
        files.push((rel, text));
    }
    validate_contents(
        files,
        secret_warnings,
        errors,
        agents,
        agent_sources,
        CompatibilityCheck::RequireNative,
    )
}

/// Validate an immutable descriptor-read snapshot before workspace publication.
pub(crate) fn validate_texts(
    files: Vec<(String, String)>,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Validated> {
    validate_snapshot(
        files,
        agents,
        agent_sources,
        CompatibilityCheck::RequireNative,
    )
}

/// Inspect immutable bytes without claiming that their target contracts are
/// implemented by Native. Unknown contract payload semantics remain target-owned.
pub(crate) fn inspect_texts(
    files: Vec<(String, String)>,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Validated> {
    validate_snapshot(
        files,
        agents,
        agent_sources,
        CompatibilityCheck::ReportNative,
    )
}

enum CompatibilityCheck {
    RequireNative,
    ReportNative,
}

fn validate_snapshot(
    files: Vec<(String, String)>,
    agents: &HashSet<String>,
    agent_sources: &[String],
    compatibility: CompatibilityCheck,
) -> Result<Validated> {
    if files.len() > MAX_FILES {
        return Err(Error::rejected("app bundle exceeds its file limit"));
    }
    if files.iter().map(|(_, text)| text.len() as u64).sum::<u64>() > MAX_APP_BYTES {
        return Err(Error::rejected(
            "app bundle exceeds its aggregate byte limit",
        ));
    }
    let mut warnings = Vec::new();
    // Portable inspection must not consult a machine's state or inherit its
    // operator exceptions. Native validation retains its existing allowlist.
    let no_host_exceptions = crate::secret::Allowlist::default();
    for (name, text) in &files {
        if text.len() as u64 > file_cap(name) {
            return Err(Error::rejected("app bundle file exceeds its limit"));
        }
        let findings = match &compatibility {
            CompatibilityCheck::RequireNative => {
                crate::secret::guard(&format!("app file {name}"), text)?
            }
            CompatibilityCheck::ReportNative => {
                crate::secret::guard_with(&format!("app file {name}"), text, &no_host_exceptions)?
            }
        };
        warnings.extend(findings);
    }
    validate_contents(
        files,
        warnings,
        Vec::new(),
        agents,
        agent_sources,
        compatibility,
    )
}

/// Storage members and their requirement are one package contract, including
/// when already-installed bytes are explicitly approved again.
pub(crate) fn validate_storage_members<'a>(
    manifest: &Manifest,
    paths: impl Iterator<Item = &'a str>,
) -> Result<()> {
    let declared = manifest.requires.contracts.get("app-storage");
    let mut root = false;
    let mut members = false;
    for path in paths {
        root |= path == "app-storage.json";
        members |= path.starts_with("storage/");
    }
    if (root || members) && declared.is_none_or(|majors| majors.as_slice() != [1]) {
        return Err(Error::rejected(
            "app storage members require needs.requires.contracts.app-storage: [1]",
        ));
    }
    if declared.is_some() && !root {
        return Err(Error::rejected("app-storage requires app-storage.json"));
    }
    Ok(())
}

fn validate_contents(
    files: Vec<(String, String)>,
    secret_warnings: Vec<crate::secret::Finding>,
    mut errors: Vec<String>,
    agents: &HashSet<String>,
    agent_sources: &[String],
    compatibility: CompatibilityCheck,
) -> Result<Validated> {
    let manifest = match files.iter().find(|(rel, _)| rel == MANIFEST) {
        Some((_, text)) => match parse_manifest(text) {
            Ok(manifest) => manifest,
            Err(e) => {
                errors.push(e.to_string());
                // Without a manifest the slot check cannot run; report
                // the rest anyway.
                Manifest {
                    app: String::new(),
                    title: String::new(),
                    version: String::new(),
                    connections: vec![],
                    capabilities: BTreeMap::new(),
                    summary: None,
                    view_contract: None,
                    requires: Compat::legacy(),
                    listing: None,
                    guide: String::new(),
                }
            }
        },
        None => Manifest {
            app: String::new(),
            title: String::new(),
            version: String::new(),
            connections: vec![],
            capabilities: BTreeMap::new(),
            summary: None,
            view_contract: None,
            requires: Compat::legacy(),
            listing: None,
            guide: String::new(),
        },
    };
    if let Err(error) =
        validate_storage_members(&manifest, files.iter().map(|(path, _)| path.as_str()))
    {
        errors.push(error.to_string());
    }
    // This bundle's own bytes as one `rel-path → text` map. The skill and
    // screen member groups are both validated over it, and run creation
    // resolves a skill package from the same shape of map.
    let bundle_map: BTreeMap<String, String> = files.iter().cloned().collect();
    // CAD-1129 §3: every asset `listing` references must be in this
    // bundle's own inventory — `assets/` flat *.svg, nothing outside.
    if let Some(listing) = &manifest.listing {
        for asset in &listing.assets {
            if !files.iter().any(|(rel, _)| rel == asset) {
                errors.push(format!(
                    "listing asset '{asset}' is not in the bundle's assets/"
                ));
            }
        }
    }
    // CAD-1110: a package whose chat descriptor breaks the app-chat/v1
    // grammar is refused at install and update, not at first use.
    if let Some((_, text)) = files
        .iter()
        .find(|(rel, _)| rel == crate::issue::app_chat::FILE)
    {
        if let Err(e) = crate::issue::app_chat::validate(text, &manifest.app) {
            errors.push(e.to_string());
        }
    }
    if let Some((_, text)) = files
        .iter()
        .find(|(rel, _)| rel == crate::issue::app_action::FILE)
    {
        if let Err(e) = crate::issue::app_action::validate_customer_bundle(text, &manifest.app) {
            errors.push(e.to_string());
        }
    }
    if let Some((_, text)) = files
        .iter()
        .find(|(rel, _)| rel == crate::issue::app_assistant::FILE)
    {
        match crate::issue::app_assistant::validate(text) {
            Ok(descriptor)
                if descriptor
                    .app
                    .as_deref()
                    .is_none_or(|app| app == manifest.app) => {}
            Ok(_) => errors.push(format!(
                "{} declares a different app name",
                crate::issue::app_assistant::FILE
            )),
            Err(e) => errors.push(format!("{}: {e}", crate::issue::app_assistant::FILE)),
        }
    }
    // CAD-864: the descriptor and its manifest declaration pair up —
    // either alone refuses. A declared `needs.views` with no
    // `views/app-views-v1.json` (or the reverse) is a bundle that
    // misstates what it ships. When both exist the descriptor parses
    // under the app-views/v1 gate and its `app` is this manifest's app:
    // the descriptor was reviewed *with* this bundle, and its
    // provenance can never borrow another app's views.
    let descriptor_file = files
        .iter()
        .find(|(rel, _)| rel == app_view::REL_PATH)
        .map(|(_, text)| text.as_str());
    match (manifest.view_contract.as_deref(), descriptor_file) {
        (Some(app_view::CONTRACT), Some(text)) => match app_view::parse_descriptor(text) {
            Ok(descriptor) if descriptor.app == manifest.app => {}
            Ok(descriptor) => errors.push(format!(
                "{}: descriptor `app` is '{}' — it must be this bundle's '{}'",
                app_view::REL_PATH,
                descriptor.app,
                manifest.app
            )),
            Err(e) => errors.push(format!("{}: {e}", app_view::REL_PATH)),
        },
        (Some(_), None) => errors.push(format!(
            "app.md declares `needs.views` but the bundle carries no {} — the \
             declaration and the descriptor file install together or not at all",
            app_view::REL_PATH
        )),
        (None, Some(_)) => errors.push(format!(
            "{} is present but app.md never declares `needs.views.contract` — an \
             undeclared descriptor can never install",
            app_view::REL_PATH
        )),
        (None, None) => {}
        // An unknown contract value already failed `parse_manifest`.
        (Some(_), Some(_)) => {}
    }
    let mut notes = Vec::new();
    let mut workflow_count = 0usize;
    for (rel, text) in &files {
        // `bundle_files` admits only `workflows/<tag>.md` under that top.
        if !rel.starts_with("workflows/") {
            continue;
        }
        workflow_count += 1;
        // CAD-1867: an installed bundle's workflow MAY declare `skill:` —
        // the tag is resolved against this bundle's own packages below, and
        // run creation re-proves it through the same `skill_package`.
        let (errs, ns, _) = workflow::check_installed_text(text, agents, agent_sources);
        errors.extend(errs.into_iter().map(|e| format!("{rel}: {e}")));
        notes.extend(ns.into_iter().map(|n| format!("{rel}: {n}")));
        if let Ok(template) = workflow::parse_template(text) {
            if let Some(slot) = template.publication_slot {
                if !manifest.capabilities.contains_key(&slot) {
                    errors.push(format!(
                        "{rel}: publication_slot '{slot}' is not declared in needs.capabilities"
                    ));
                }
            }
        }
        match workflow_slots(text) {
            Ok(slots) => {
                for slot in slots {
                    if !manifest.connections.iter().any(|s| s == &slot) {
                        errors.push(format!(
                            "{rel}: `uses: {slot}` — the app does not declare that slot; \
                             add it to `needs.connections` in app.md so the binding is \
                             the operator's choice, not the bundle's"
                        ));
                    }
                }
            }
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
        match workflow::skill_tags(text) {
            Ok(tags) => {
                for tag in tags.into_iter().flatten() {
                    if let Err(e) = skill_package(&bundle_map, &tag) {
                        errors.push(format!("{rel}: `skill: {tag}` — {e}"));
                    }
                }
            }
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
    }
    // CAD-1867: every `skills/<tag>/` package the bundle carries must be
    // complete on its own terms — its manifest, the declared set equal to the
    // present set, the caps, and every reference its declared text files make
    // to package material. One validator, `skill_package`, so install and run
    // creation can never disagree.
    match skill_packages(&bundle_map) {
        Ok(_) => {}
        Err(e) => errors.push(e.to_string()),
    }
    // CAD-1270 item 3: every `screens/<tag>/` member group is a screen
    // package — integrity-check each through the same
    // `app_screen_pkg::extract` → `app_screen_decl::validate_map` the
    // mount RPC re-runs, so a malformed declaration, an
    // undeclared/tampered asset or a tag dir without `screens.json`
    // refuses at install, not first mount. `tags_in` additionally
    // enforces the ≤8 declared-package bound; the member-prefix scan
    // also collects an orphan tag dir (no screens.json), which
    // `tags_in` cannot see. This shared check is what install,
    // install-check and the offline `app check` all inherit — no second
    // parser.
    {
        let screen_map = &bundle_map;
        if let Err(e) = crate::issue::app_screen_pkg::tags_in(screen_map) {
            errors.push(format!("screens: {e}"));
        }
        let mut candidates: Vec<&str> = Vec::new();
        for rel in screen_map.keys() {
            if let Some(rest) = rel.strip_prefix("screens/") {
                if let Some((tag, _)) = rest.split_once('/') {
                    if !candidates.contains(&tag) {
                        candidates.push(tag);
                    }
                }
            }
        }
        for tag in candidates {
            match crate::issue::app_screen_pkg::extract(screen_map, tag) {
                // Same binding the mount RPC enforces: the package's
                // declared `app` must be this bundle's manifest app —
                // a package cannot claim a different app.
                Ok(pkg) if pkg.app != manifest.app => errors.push(format!(
                    "screens/{tag}: screen package declares an app that is not the installation"
                )),
                Err(e) => errors.push(format!("screens/{tag}: {e}")),
                Ok(_) => {}
            }
        }
    }
    // CAD-1177: an app may ship no workflows only when it is a genuine
    // standalone-tools plugin — it carries at least one integrity-checked
    // `app-screens/v2` screen whose declared `tools` map resolves every
    // alias to a real `needs.capabilities` slot. That is the v2
    // no-workflow-file path: the tool, not a run, is the unit of work.
    // An empty app, a screen that fails integrity, or a tool whose slot
    // the manifest never declares cannot substitute for workflows and
    // still refuses.
    if workflow_count == 0 && !has_tools_only_screen(&files, &manifest) {
        errors.push("workflows/ holds no *.md workflow".to_string());
    }
    let host = app_compat::host(env!("CARGO_PKG_VERSION"))?;
    let compatibility = match compatibility {
        CompatibilityCheck::ReportNative => manifest.requires.report(&host),
        CompatibilityCheck::RequireNative => match manifest.requires.check(&host) {
            Ok(receipt) => receipt,
            Err(error) => {
                errors.push(error.to_string());
                Value::Null
            }
        },
    };
    if !errors.is_empty() {
        return Err(Error::rejected(format!(
            "app is not installable — {}",
            errors.join("; ")
        )));
    }
    Ok(Validated {
        manifest,
        files,
        secret_warnings,
        notes,
        compatibility,
    })
}

/// CAD-1177: whether the bundle carries a genuine standalone-tools
/// screen — an integrity-checked `app-screens/v2` package whose
/// declared `tools` map is non-empty AND resolves every alias to a
/// `needs.capabilities` slot with a `read`/`draft` effect (the only
/// effects a standalone, approval-free invoke may run). Its `may` is
/// `tools.invoke`, optionally plus the exact `media.read` permission a
/// `screen-media.v1` screen declares (CAD-1365). This is the
/// install-time proof that lets a workflow-free plugin install; it is
/// deliberately strict so an empty or malformed bundle cannot borrow
/// the exception. Returns false on any integrity/parse failure — a
/// broken screen never satisfies the exception (its own error is
/// reported by the screen validators elsewhere).
fn has_tools_only_screen(files: &[(String, String)], manifest: &Manifest) -> bool {
    let map: BTreeMap<String, String> = files.iter().cloned().collect();
    let Ok(tags) = crate::issue::app_screen_pkg::tags_in(&map) else {
        return false;
    };
    tags.iter().any(|tag| {
        let Ok(pkg) = crate::issue::app_screen_pkg::extract(&map, tag) else {
            return false;
        };
        pkg.contract == "app-screens/v2"
            // The screen must be FOR this app — a package declaring a
            // different app can never satisfy this bundle's exception.
            && pkg.app == manifest.app
            && !pkg.tools.is_empty()
            // `tools.invoke` alone, plus — only under the media host
            // contract — the exact `media.read`/`media.publish`
            // declarations. No other `may` member and no media right on
            // an actions-contract screen qualifies.
            && pkg.may.iter().all(|m| {
                m == crate::issue::app_screen_decl::MAY_TOOLS_INVOKE
                    || ((m == crate::issue::app_screen_decl::MAY_MEDIA_READ
                        || m == crate::issue::app_screen_decl::MAY_MEDIA_PUBLISH)
                        && pkg.host_contract.as_deref()
                            == Some(crate::issue::app_screen_decl::HOST_CONTRACT_MEDIA))
            })
            && pkg.tools.values().all(|slot| {
                manifest
                    .capabilities
                    .get(slot)
                    .is_some_and(|decl| matches!(decl.effect.as_str(), "read" | "draft"))
            })
    })
}

/// `mkdir` that never follows a link: `create_dir` is EEXIST on any
/// existing entry — a planted symlink included — and an existing entry
/// is accepted only when it is a real directory (N5).
fn mkdir_verified(dir: &Path) -> Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match dir.symlink_metadata() {
            Ok(m) if m.is_symlink() => Err(Error::rejected(format!(
                "{} is a symlink — the tracker never follows links",
                dir.display()
            ))),
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(Error::rejected(format!(
                "{} exists and is not a directory",
                dir.display()
            ))),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

/// Write `text` to `file` without ever following a planted link —
/// `O_NOFOLLOW` makes the open refuse one outright (N5).
fn write_verified(file: &Path, text: &str) -> Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(file)?;
    std::io::Write::write_all(&mut f, text.as_bytes())?;
    Ok(f)
}

/// Copy a verified bundle into `target`, 0644 files — no mode bits,
/// exec or otherwise, travel with app content.
fn copy_verified(files: &[(String, String)], target: &Path) -> Result<()> {
    // The install/update target is itself part of the checked tree. Do not
    // assume it is safe merely because its caller validated the bundle.
    mkdir_verified(target)?;
    for (rel, text) in files {
        let relative = Path::new(rel);
        let components: Vec<_> = relative.components().collect();
        if components.is_empty()
            || components
                .iter()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(Error::rejected(format!(
                "{} is not a normalized relative bundle path",
                rel
            )));
        }

        let mut to = target.to_path_buf();
        for component in &components[..components.len() - 1] {
            to.push(component.as_os_str());
            mkdir_verified(&to)?;
        }
        to.push(components[components.len() - 1].as_os_str());
        let f = write_verified(&to, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The structural digest `app approve` records and `plan propose
/// --workflow <app>/<wf>` matches: app name, declared slots, each
/// slot's effective binding, the guide body, every workflow's CAD-487
/// gate digest, and a content hash of each rubric/template file — the
/// whole envelope the operator reviewed, computed over the INSTALLED
/// dir (never a source it claims to match). Any structural change —
/// install or hand edit — changes it, which is what re-gates the app.
pub fn digest(pm_dir: &Path, project: &str, name: &str) -> Result<String> {
    digest_over(pm_dir, project, name, &[])
}

/// `digest` with caller-held workflow texts: for each `(rel, text)` in
/// `over`, the entry `wf.<x>=` hashes `text` instead of re-reading the
/// file — a caller that renders a buffer it already read proves the
/// approval covered THOSE bytes, never a second read a co-host writer
/// could have flipped (N2: plan_text renders what it digests).
fn digest_over(pm_dir: &Path, project: &str, name: &str, over: &[(&str, &str)]) -> Result<String> {
    let dir = app_dir(pm_dir, project, name)?;
    let record = read_record(pm_dir, project, name)?;
    let text = std::fs::read_to_string(dir.join(MANIFEST)).map_err(|e| {
        Error::rejected(format!("cannot read {}: {e}", dir.join(MANIFEST).display()))
    })?;
    let manifest = parse_manifest(&text)
        .map_err(|e| Error::rejected(format!("installed {}: {e}", dir.join(MANIFEST).display())))?;
    // The installed layout is walked with the same strict rules as a
    // source — a symlink planted post-install makes the app undigestable
    // rather than silently outside the digest.
    let files = bundle_files(&dir)?;
    let mut keys = format!("app={}\n", manifest.app);
    keys.push_str(&format!("slots={}\n", manifest.connections.join(",")));
    if !manifest.capabilities.is_empty() {
        keys.push_str(&format!(
            "capabilities={}\n",
            serde_json::to_string(&manifest.capabilities)?
        ));
    }
    keys.push_str(&manifest.requires.digest_line());
    for slot in &manifest.connections {
        keys.push_str(&format!(
            "bind.{slot}={}\n",
            record.binding(slot).unwrap_or("~")
        ));
    }
    keys.push_str(&format!(
        "guide={}\n",
        sha256_hex(manifest.guide.as_bytes())
    ));
    for (rel, path) in &files {
        if rel == MANIFEST {
            continue;
        }
        if let Some(wf) = rel
            .strip_prefix("workflows/")
            .and_then(|n| n.strip_suffix(".md"))
        {
            let text: std::borrow::Cow<'_, str> = match over.iter().find(|(r, _)| r == rel) {
                Some((_, t)) => std::borrow::Cow::Borrowed(*t),
                None => std::borrow::Cow::Owned(
                    std::fs::read_to_string(path)
                        .map_err(|e| Error::rejected(format!("cannot read {rel}: {e}")))?,
                ),
            };
            keys.push_str(&format!("wf.{wf}={}\n", workflow::gate_digest(&text)?));
        } else {
            let bytes = std::fs::read(path)
                .map_err(|e| Error::rejected(format!("cannot read {rel}: {e}")))?;
            keys.push_str(&format!("file.{rel}={}\n", sha256_hex(&bytes)));
        }
    }
    Ok(format!("sha256:{}", sha256_hex(keys.as_bytes())))
}

/// Every recorded app approval: `"<project>/<app>"` → payload. Read
/// straight from the store, read-only — like the workflow approvals —
/// so `ls`/`show`/`check` work with the daemon down; `None` means absent
/// or unreadable, and readers show approval as unknown, never granted.
pub fn fetch_approvals(state_dir: &Path) -> Option<Map<String, Value>> {
    let path = state_dir.join("cadence.sqlite3");
    if !path.exists() {
        return None;
    }
    let conn = crate::store::open_read_only(&path).ok()?;
    let mut stmt = conn
        .prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")
        .ok()?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                crate::store::APPROVAL_STREAM,
                crate::store::APP_APPROVED_EVENT
            ],
            |r| r.get::<_, String>(0),
        )
        .ok()?;
    let mut out = Map::new();
    for raw in rows.flatten() {
        let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let (Some(p), Some(n)) = (payload["project"].as_str(), payload["name"].as_str()) {
            out.insert(approval_key(p, n), payload);
        }
    }
    Some(out)
}

/// The key an approval is stored under — `"<project>/<app>"`.
pub fn approval_key(project: &str, name: &str) -> String {
    format!("{project}/{name}")
}

/// A fresh id for one install. `app update` keeps whatever install
/// already wrote; remove deletes the record, so the next install mints
/// another.
fn mint_install_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The install id on the record, or empty when the record has none
/// (a pre-install-id yaml) or cannot be read.
pub fn current_install_id(pm_dir: &Path, project: &str, name: &str) -> String {
    read_record(pm_dir, project, name)
        .map(|r| r.install_id)
        .unwrap_or_default()
}

/// Give a legacy record an install id and commit it. The caller already
/// holds `pm`'s write lock. A record that already has an id is unchanged.
pub fn ensure_install_id(pm: &Pm, project: &str, name: &str, actor: &str) -> Result<String> {
    let mut record = read_record(&pm.dir, project, name)?;
    if !record.install_id.is_empty() {
        return Ok(record.install_id);
    }
    record.install_id = mint_install_id();
    let path = record_file(&pm.dir, project, name)?;
    let _legacy = pm.begin_legacy_write()?;
    write_record(&path, &record)?;
    write::commit(
        pm,
        &[path],
        &format!("{project}/{DIR}/{name}: install id assigned"),
        &[],
        actor,
    )?;
    Ok(record.install_id)
}

/// An approval covers this install: not withdrawn, the digest is the
/// one just computed, and `install_id` is the current record's id.
/// A missing or empty id never matches — an approval from before
/// install ids, or from a previous install of the same bytes, does not
/// carry over.
pub fn approval_binds(payload: &Value, digest: &str, install_id: &str) -> bool {
    if install_id.is_empty() {
        return false;
    }
    if payload.get("revoked").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    payload.get("digest").and_then(Value::as_str) == Some(digest)
        && payload.get("install_id").and_then(Value::as_str) == Some(install_id)
}

/// The Apps board's approval word (CAD-557) — why `approved` is false:
/// `approved`, `changed` (this install's record exists but the digest
/// moved — any bundle or binding edit re-gates the app), `unapproved`
/// (no record, or the record belongs to another install), or `unknown`
/// (the approval store did not read — never shown granted).
fn approval_state(
    approvals: Option<&Map<String, Value>>,
    key: &str,
    digest: &str,
    install_id: &str,
) -> &'static str {
    let Some(approvals) = approvals else {
        return "unknown";
    };
    match approvals.get(key) {
        Some(p) if approval_binds(p, digest, install_id) => "approved",
        Some(p)
            if !install_id.is_empty()
                && p.get("revoked").and_then(Value::as_bool) != Some(true)
                && p.get("install_id").and_then(Value::as_str) == Some(install_id) =>
        {
            "changed"
        }
        Some(_) => "unapproved",
        None => "unapproved",
    }
}

/// Is `digest` the approved digest for this install of `project/app`?
/// An unreachable daemon is no approval — fail closed, like `plan
/// propose` refusing. A missing install id is not a match.
pub fn approved(
    project: &str,
    name: &str,
    digest: &str,
    install_id: &str,
    approvals: Option<&Map<String, Value>>,
) -> bool {
    approvals
        .and_then(|a| a.get(&approval_key(project, name)))
        .is_some_and(|p| approval_binds(p, digest, install_id))
}

/// `<app>/<workflow>` → both parts, tag-shaped; `None` for a bare name
/// or a malformed ref (a second `/`, an empty half).
pub fn split_ref(name: &str) -> Option<(&str, &str)> {
    let (app, wf) = name.split_once('/')?;
    if model::valid_tag(app) && model::valid_tag(wf) && !wf.contains('/') {
        Some((app, wf))
    } else {
        None
    }
}

/// The installed `apps/<app>/workflows/<wf>.md` — real file only.
pub fn read_workflow(pm_dir: &Path, project: &str, app: &str, wf: &str) -> Result<String> {
    check_name(wf, "workflow name")?;
    let file = app_dir(pm_dir, project, app)?
        .join("workflows")
        .join(format!("{wf}.md"));
    if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            file.display()
        )));
    }
    std::fs::read_to_string(&file).map_err(|_| {
        Error::rejected(format!(
            "no workflow '{wf}' in app '{app}' ({project}) — `cadence app show {app} \
             --project {project}` lists its workflows"
        ))
    })
}

/// The git source resolved and pinned: clone into a private temp dir
/// (never executed, never trusted past the SHA we record), answer the
/// clone root and the commit SHA `rev-parse HEAD` reads.
fn clone_git(url: &str, into: &Path) -> Result<String> {
    let out = crate::reaper::output(
        std::process::Command::new("timeout")
            .arg("120")
            .arg("git")
            .args(["clone", "--quiet", "--no-tags", "--depth", "1", "--"])
            .arg(url)
            .arg(into)
            .env("GIT_TERMINAL_PROMPT", "0")
            // `--` keeps a dash-led source a repository name, never a
            // switch; transports are pinned to https/ssh/file (a local
            // dir installs as a path, never through clone) — no `ext::`
            // or other transport ever runs (N3).
            .env("GIT_ALLOW_PROTOCOL", "https:ssh:file"),
    )
    .map_err(|e| Error::rejected(format!("git clone of '{url}' failed to run: {e}")))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git clone of '{url}' failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let head = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(into)
            .args(["rev-parse", "HEAD"]),
    )
    .map_err(|e| Error::rejected(format!("git rev-parse in the clone failed: {e}")))?;
    let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if !head.status.success()
        || sha.len() != 40
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::rejected(format!(
            "git clone of '{url}' gave no commit SHA — the install pins one; \
             the clone may be empty"
        )));
    }
    Ok(sha)
}

/// Resolve `<path|git-url>` to `(dir to scan, source record)`. An
/// existing directory is a path install; anything else is tried as a
/// git URL (a mistyped path fails the clone with a clear error). The
/// returned TempDir, when `Some`, owns the clone — keep it alive until
/// the copy has landed.
pub(crate) fn resolve_source(source: &str) -> Result<(PathBuf, Source, Option<tempfile::TempDir>)> {
    let path = Path::new(source);
    if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "app source {source} is a symlink — an app is a real folder"
        )));
    }
    if path.is_dir() {
        let canon = path
            .canonicalize()
            .map_err(|e| Error::rejected(format!("cannot resolve app source {source}: {e}")))?;
        return Ok((
            canon.clone(),
            Source::Path {
                path: canon.display().to_string(),
            },
            None,
        ));
    }
    if path.exists() {
        return Err(Error::rejected(format!(
            "app source {source} is a file — it names a folder or a git url"
        )));
    }
    let tmp = tempfile::tempdir()?;
    let clone_dir = tmp.path().join("clone");
    let sha = clone_git(source, &clone_dir)?;
    Ok((
        clone_dir,
        Source::Git {
            url: source.to_string(),
            sha,
            dir: None,
        },
        Some(tmp),
    ))
}

/// `cadence app install <path|git-url> --project <key>`: verify the
/// bundle (every workflow through the same `workflow check`), copy it
/// to `<pm>/<key>/apps/<name>/`, write `apps/<name>.yaml`, and land it
/// UNAPPROVED — one tracker commit, `Actor:` recorded. Nothing in the
/// bundle executes; a git source pins its HEAD SHA into the record.
pub fn install(
    pm: &Pm,
    project_key: &str,
    source: &str,
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    model::check_key(project_key)?;
    if !project::list(&pm.dir)?.iter().any(|p| p.key == project_key) {
        return Err(project::unknown_project(project_key, &pm.dir));
    }
    let (src_dir, src, _tmp) = resolve_source(source)?;
    // The tracker is never its own source — `apps/` inside it included.
    let canon_pm = pm.dir.canonicalize().unwrap_or_else(|_| pm.dir.clone());
    if src_dir.starts_with(&canon_pm) {
        return Err(Error::rejected(format!(
            "app source {source} is inside the tracker {} — install a copy \
             from outside it",
            pm.dir.display()
        )));
    }
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) =
        workflow::known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let Validated {
        manifest,
        files,
        secret_warnings,
        notes,
        ..
    } = validate(&src_dir, &agents, &agent_sources)?;
    let name = manifest.app.clone();
    let apps = dir_of(&pm.dir, project_key)?;
    let target = apps.join(&name);
    let record_path = apps.join(format!("{name}.yaml"));
    let _lock = pm.lock()?;
    let _legacy = pm.begin_legacy_write()?;
    for path in [&target, &record_path] {
        match path.symlink_metadata() {
            Ok(m) if m.is_symlink() => {
                return Err(Error::rejected(format!(
                    "{} is a symlink — the tracker never follows links",
                    path.display()
                )));
            }
            Ok(_) => {
                return Err(Error::rejected(format!(
                    "app '{name}' is already installed in {project_key} — `cadence \
                     app update {name} --project {project_key}` replaces it"
                )));
            }
            Err(_) => {}
        }
    }
    // mkdir, never create_dir_all: a leaf planted between the check and
    // here — a symlink included — is EEXIST, not a dir the copy writes
    // into; then what exists is verified a real directory (N5).
    mkdir_verified(&apps)?;
    mkdir_verified(&target)?;
    match target.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.is_symlink() => {}
        _ => {
            let _ = std::fs::remove_dir_all(&target);
            return Err(Error::rejected(format!(
                "{} was moved or replaced during install — refusing",
                target.display()
            )));
        }
    }
    let rollback = |keep_record: bool| {
        let _ = std::fs::remove_dir_all(&target);
        if !keep_record {
            let _ = std::fs::remove_file(&record_path);
        }
    };
    if let Err(e) = copy_verified(&files, &target) {
        rollback(false);
        return Err(e);
    }
    let now = crate::issue::time::iso(crate::issue::time::now_epoch());
    let record = Record {
        schema: 1,
        app: name.clone(),
        install_id: mint_install_id(),
        source: src.clone(),
        bindings: BTreeMap::new(),
        team: BTreeMap::new(),
        installed_at: now,
        installed_by: write::actor_who(actor, None),
        updated_at: None,
        removed: None,
        restore_after: None,
        purge_after: None,
        catalogue: None,
    };
    if let Err(e) = write_record(&record_path, &record) {
        rollback(false);
        return Err(e);
    }
    let label = match &src {
        Source::Path { path } => format!("path {path}"),
        Source::Git { url, sha, .. } => format!("git {url} @ {}", &sha[..12]),
        Source::Builtin { id, .. } => format!("builtin {id}"),
    };
    let foreign = match write::commit(
        pm,
        &[target.clone(), record_path.clone()],
        &format!("{project_key}/{DIR}/{name}: app installed ({label})"),
        &[],
        actor,
    ) {
        Ok(foreign) => foreign,
        Err(e) => {
            rollback(false);
            return Err(e);
        }
    };
    let digest = digest(&pm.dir, project_key, &name)?;
    let mut out = json!({
        "project": project_key,
        "name": name,
        "path": target,
        "title": manifest.title,
        "version": manifest.version,
        "connections": manifest.connections,
        "workflows": files.iter().filter_map(|(rel, _)| rel
            .strip_prefix("workflows/").and_then(|n| n.strip_suffix(".md"))
            .map(str::to_string)).collect::<Vec<_>>(),
        "digest": digest,
        "install_id": record.install_id,
        "approved": false,
        "note": "unapproved — nothing in the app runs until the operator's \
                 `cadence app approve`",
        "source": serde_json::to_value(&src).unwrap_or(Value::Null),
        "committed": true,
        "notes": notes,
    });
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// The diff `app update` prints — `added`/`removed`/`changed` rel-paths
/// plus a unified `patch` (`git diff --no-index`, `<app>` as both roots)
/// capped at 64 KiB.
fn diff_report(name: &str, old_dir: &Path, new_files: &[(String, String)]) -> Result<Value> {
    let old: BTreeMap<String, String> = bundle_files(old_dir)?
        .into_iter()
        .filter_map(|(rel, path)| std::fs::read_to_string(&path).ok().map(|t| (rel, t)))
        .collect();
    let new: BTreeMap<String, String> = new_files.iter().cloned().collect();
    let added: Vec<&String> = new.keys().filter(|k| !old.contains_key(*k)).collect();
    let removed: Vec<&String> = old.keys().filter(|k| !new.contains_key(*k)).collect();
    let changed: Vec<&String> = new
        .iter()
        .filter(|(k, v)| old.get(*k).is_some_and(|o| o != *v))
        .map(|(k, _)| k)
        .collect();
    let tmp = tempfile::tempdir()?;
    let (old_root, new_root) = (tmp.path().join("old"), tmp.path().join("new"));
    for (rel, text) in &old {
        let to = old_root.join(rel);
        std::fs::create_dir_all(to.parent().unwrap())?;
        std::fs::write(to, text)?;
    }
    for (rel, text) in &new {
        let to = new_root.join(rel);
        std::fs::create_dir_all(to.parent().unwrap())?;
        std::fs::write(to, text)?;
    }
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["diff", "--no-index", "--", "old", "new"]),
    )?;
    // --no-index exits 1 on differences, 2+ on a real failure.
    if !matches!(out.status.code(), Some(0) | Some(1)) {
        return Err(Error::internal(format!(
            "git diff --no-index failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut patch = String::from_utf8_lossy(&out.stdout).to_string();
    patch = patch.replace("a/old/", &format!("a/{name}/"));
    patch = patch.replace("b/new/", &format!("b/{name}/"));
    const CAP: usize = 64 * 1024;
    if patch.len() > CAP {
        let mut cap = CAP;
        while !patch.is_char_boundary(cap) {
            cap -= 1;
        }
        patch.truncate(cap);
        patch.push_str("\n… patch truncated at 64 KiB");
    }
    Ok(json!({
        "added": added,
        "removed": removed,
        "changed": changed,
        "patch": patch,
    }))
}

/// `cadence app update <app> [path|git-url] --project <key>` — replace
/// the installed bundle from the recorded (or a given) source, after
/// the same verification install runs. Prints the diff; bindings that
/// still name declared slots survive, the rest are dropped (named in
/// the output). Any structural change — a slot, a binding carried over
/// or not, a workflow gate key, a guide/rubric/template byte — changes
/// the digest, which is what re-requires approval: nothing extra to
/// revoke.
pub fn update(
    pm: &Pm,
    project_key: &str,
    name: &str,
    source_arg: Option<&str>,
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    model::check_key(project_key)?;
    check_name(name, "app name")?;
    let dir = app_dir(&pm.dir, project_key, name)?;
    let record = read_record(&pm.dir, project_key, name)?;
    let old_digest = digest(&pm.dir, project_key, name)?;
    let old_version = read_manifest(&pm.dir, project_key, name)
        .map(|m| m.version)
        .unwrap_or_default();
    let (src_dir, src, _tmp) = match source_arg {
        Some(arg) => resolve_source(arg)?,
        None => match &record.source {
            Source::Path { path } => resolve_source(path)?,
            Source::Git { url, .. } => resolve_source(url)?,
            Source::Builtin { .. } => {
                return Err(Error::rejected(
                    "a built-in app updates from the catalog's Check update, not a CLI path",
                ))
            }
        },
    };
    if src_dir.starts_with(pm.dir.canonicalize().unwrap_or_else(|_| pm.dir.clone())) {
        return Err(Error::rejected(format!(
            "app source {} is inside the tracker — update from outside it",
            src_dir.display()
        )));
    }
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) =
        workflow::known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let Validated {
        manifest,
        files,
        secret_warnings,
        notes,
        ..
    } = validate(&src_dir, &agents, &agent_sources)?;
    if manifest.app != name {
        return Err(Error::rejected(format!(
            "the bundle names app '{}' — `app update {name}` replaces '{name}' \
             only; install '{}' separately",
            manifest.app, manifest.app
        )));
    }
    let diff = diff_report(name, &dir, &files)?;
    // Slot bindings carry over only where the new manifest still
    // declares the slot; dropped ones are named, not silently kept.
    let mut bindings = record.bindings.clone();
    let dropped: Vec<String> = bindings
        .keys()
        .filter(|k| !manifest.connections.iter().any(|s| s == *k))
        .cloned()
        .collect();
    for slot in &dropped {
        bindings.remove(slot);
    }
    let _lock = pm.lock()?;
    let _legacy = pm.begin_legacy_write()?;
    // Replace: the dir is our own verified layout — remove the files
    // the diff says are gone, then write the new set. A failure mid-way
    // leaves a partially-updated dir that fails digest — unusable until
    // the operator re-runs update or remove, never silently approved.
    for rel in diff["removed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
    {
        let _ = std::fs::remove_file(dir.join(rel));
    }
    // A top dir the update emptied is removed too — an empty rubrics/
    // is lint-clean but dead weight.
    for top in TOP_DIRS {
        let sub = dir.join(top);
        if sub.is_dir()
            && std::fs::read_dir(&sub)
                .map(|mut e| e.next().is_none())
                .unwrap_or(false)
            && !files
                .iter()
                .any(|(rel, _)| rel.starts_with(&format!("{top}/")))
        {
            let _ = std::fs::remove_dir(&sub);
        }
    }
    copy_verified(&files, &dir)?;
    let mut record = record;
    record.source = src;
    record.bindings = bindings;
    // A legacy record has no id. Mint once here so later updates keep
    // it; an approval that predates install ids still does not match.
    if record.install_id.is_empty() {
        record.install_id = mint_install_id();
    }
    record.updated_at = Some(crate::issue::time::iso(crate::issue::time::now_epoch()));
    let record_path = record_file(&pm.dir, project_key, name)?;
    write_record(&record_path, &record)?;
    let label = match &record.source {
        Source::Path { path } => format!("path {path}"),
        Source::Git { url, sha, .. } => format!("git {url} @ {}", &sha[..12]),
        Source::Builtin { id, .. } => format!("builtin {id}"),
    };
    let foreign = write::commit(
        pm,
        &[dir.clone(), record_path],
        &format!("{project_key}/{DIR}/{name}: app updated ({label})"),
        &[],
        actor,
    )?;
    let digest = digest(&pm.dir, project_key, name)?;
    let approvals = fetch_approvals(state_dir);
    let approved = approved(
        project_key,
        name,
        &digest,
        &record.install_id,
        approvals.as_ref(),
    );
    let mut out = json!({
        "project": project_key,
        "name": name,
        "version": {"from": old_version, "to": manifest.version},
        "diff": diff,
        "dropped_bindings": dropped,
        "digest": digest,
        "approved": approved,
        "gate_changed": old_digest != digest,
        "committed": true,
        "notes": notes,
    });
    if old_digest != digest {
        out["unapproved"] = json!(
            "structural change — `cadence app approve {name} --project {project_key}` \
             before anything in the app runs"
        );
    }
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// Epic ids of plans a `plan propose --workflow <name>/…` created that
/// are still open — `state` not `rejected` and the epic not
/// done/dropped. The tracker provenance (`plan.workflow`) is the record;
/// the daemon's `plan_proposed` events are the fallback for epics whose
/// frontmatter lost the field to an older binary's rewrite.
fn open_plan_epics(pm_dir: &Path, project_key: &str, name: &str, state_dir: &Path) -> Vec<String> {
    let mut open: Vec<String> = Vec::new();
    let is_open = |front: &model::Front| -> bool {
        let Some(plan) = &front.plan else {
            return false;
        };
        plan.state != "rejected" && !matches!(front.status.as_str(), "done" | "dropped")
    };
    // `<app>/<wf>` only — a stored workflow that happens to share the
    // app's name (`plan.workflow = "studio"`, no slash) is not its plan.
    let from_app = |wf: &str| split_ref(wf).is_some_and(|(app, _)| app == name);
    if let Ok(issues) = board::load_all(pm_dir, Some(project_key)) {
        for issue in &issues {
            if let Some(wf) = issue
                .front
                .plan
                .as_ref()
                .and_then(|p| p.workflow.as_deref())
            {
                if from_app(wf) && is_open(&issue.front) {
                    open.push(issue.front.id.clone());
                }
            }
        }
    }
    // Fallback provenance: `plan_proposed` payloads carry `app`.
    let db = state_dir.join("cadence.sqlite3");
    if db.exists() {
        if let Ok(conn) = crate::store::open_read_only(&db) {
            if let Ok(mut stmt) =
                conn.prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")
            {
                if let Ok(rows) = stmt.query_map(
                    rusqlite::params![crate::store::Store::DAEMON_STREAM, "plan_proposed"],
                    |r| r.get::<_, String>(0),
                ) {
                    for raw in rows.flatten() {
                        let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
                            continue;
                        };
                        if payload["app"].as_str() != Some(name)
                            || payload["project"].as_str() != Some(project_key)
                        {
                            continue;
                        }
                        let Some(epic) = payload["epic"].as_str() else {
                            continue;
                        };
                        if model::check_id(epic).is_err() || open.iter().any(|id| id == epic) {
                            continue;
                        }
                        // Only an epic that still exists and is still
                        // open blocks removal.
                        if let Ok((front, _)) = parse::parse_issue(
                            &std::fs::read_to_string(
                                pm_dir.join(project_key).join(epic).join("issue.md"),
                            )
                            .unwrap_or_default(),
                        ) {
                            if is_open(&front) {
                                open.push(epic.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    open.sort();
    open.dedup();
    open
}

/// The agents holding a grant this app's approval derived, read from
/// the daemon's store read-only (CAD-577). Empty when there is no
/// store yet — no daemon, no derived grant. A store that exists but
/// cannot be read refuses: "no holders" would let removal delete the
/// folder and leave the grant behind.
fn derived_grant_holders(project_key: &str, name: &str, state_dir: &Path) -> Result<Vec<String>> {
    let db = state_dir.join("cadence.sqlite3");
    if !db.exists() {
        return Ok(Vec::new());
    }
    let refuse = |why: String| {
        Error::rejected(format!(
            "app '{name}' cannot be removed — its derived grants could not be read \
             ({why}). Fix the store, or revoke first (`cadence app revoke {name} \
             --project {project_key}`)"
        ))
    };
    let conn = crate::store::open_read_only(&db).map_err(|e| refuse(e.to_string()))?;
    let mut stmt = conn
        .prepare("SELECT DISTINCT agent FROM app_grants WHERE app=? ORDER BY agent")
        .map_err(|e| refuse(e.to_string()))?;
    let key = approval_key(project_key, name);
    let rows = stmt
        .query_map(rusqlite::params![key], |r| r.get::<_, String>(0))
        .map_err(|e| refuse(e.to_string()))?;
    let mut holders = Vec::new();
    for row in rows {
        holders.push(row.map_err(|e| refuse(e.to_string()))?);
    }
    Ok(holders)
}

/// Withdraw the approval and its derived grants before the folder goes
/// (CAD-577). The caller holds the tracker write lock across this read,
/// the side write, and the folder delete — tracker lock, then sqlite,
/// the same order as approve. A missing store means there is nothing to
/// revoke. The withdrawal is by `install_id` and happens even when the
/// approval derived no grant rows — an empty holder list used to leave
/// the approval in place. If the store exists but cannot be read,
/// removal refuses.
fn revoke_derived_on_remove(
    project_key: &str,
    name: &str,
    install_id: &str,
    state_dir: &Path,
    actor: &str,
) -> Result<()> {
    let db = state_dir.join("cadence.sqlite3");
    if !db.exists() {
        return Ok(());
    }
    // Fail closed: a store that exists but cannot be read is not "no
    // holders". The read runs even when it returns an empty list.
    let holders = derived_grant_holders(project_key, name, state_dir)?;
    let refuse = |why: &str| {
        let what = if holders.is_empty() {
            "its approval".to_string()
        } else {
            format!("the grants its approval derived ({})", holders.join(", "))
        };
        Error::rejected(format!(
            "app '{name}' still holds {what} — revoke them first \
             (`cadence app revoke {name} --project {project_key}`): {why}"
        ))
    };
    let store = crate::store::Store::open_side(&db).map_err(|e| refuse(&e.to_string()))?;
    let by = if actor.is_empty() { "cli" } else { actor };
    let payload = json!({
        "project": project_key,
        "name": name,
        "install_id": install_id,
        "digest": Value::Null,
        "revoked": true,
        "by": by,
        "at": crate::issue::time::iso(crate::issue::time::now_epoch()),
        "reason": "removed",
    });
    store
        .app_revoke_with_record(payload, &approval_key(project_key, name), by)
        .map_err(|e| refuse(&e.to_string()))?;
    Ok(())
}

/// `cadence app remove <app> --project <key>` — delete the content
/// folder and its install record in one tracker commit. Refuses while a
/// plan proposed from the app is still open — an open plan's tickets
/// keep their provenance readable. Revokes the approval's derived
/// grants first (CAD-577): deleting the folder is a tracker write the
/// daemon does not see, so the grants are withdrawn here, on a side
/// connection that does not run restart recovery. The tracker write
/// lock is taken before that read and held through the folder delete,
/// so an approve already inside the lock cannot commit a grant afterwards.
pub fn remove(
    pm: &Pm,
    project_key: &str,
    name: &str,
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    model::check_key(project_key)?;
    let dir = app_dir(&pm.dir, project_key, name)?;
    let record_path = record_file(&pm.dir, project_key, name)?;
    let open = open_plan_epics(&pm.dir, project_key, name, state_dir);
    if !open.is_empty() {
        return Err(Error::rejected(format!(
            "app '{name}' has open plans ({}) — decide or finish them first \
             (`cadence plan ls --project {project_key}`)",
            open.join(", ")
        )));
    }
    // Before the holder read, and held through the revoke and the
    // folder delete. Approve takes this lock and then writes sqlite;
    // reading holders first lets that approve commit into the gap.
    let _lock = pm.lock()?;
    let _legacy = pm.begin_legacy_write()?;
    let install_id = current_install_id(&pm.dir, project_key, name);
    revoke_derived_on_remove(project_key, name, &install_id, state_dir, actor)?;
    std::fs::remove_dir_all(&dir)?;
    if record_path.exists() {
        std::fs::remove_file(&record_path)?;
    }
    let foreign = write::commit(
        pm,
        &[dir, record_path],
        &format!("{project_key}/{DIR}/{name}: app removed"),
        &[],
        actor,
    )?;
    let mut out = json!({
        "project": project_key,
        "name": name,
        "removed": true,
        "committed": true,
    });
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// `cadence app set <app> <slot>=<connection> […] --project <key>` —
/// record slot bindings in the install record; `<slot>=` unbinds
/// explicitly (neither bound nor defaulted — `doctor` reports it). A
/// slot the manifest does not declare refuses; a connection name the
/// daemon does not register lands with a warning (it may not exist yet
/// — `doctor` reports it as unknown). Because bindings are in the gate
/// digest, every `set` re-gates the app.
pub fn set(
    pm: &Pm,
    project_key: &str,
    name: &str,
    bindings: &[String],
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    if bindings.is_empty() {
        return Err(Error::rejected(
            "app set needs <slot>=<connection> — `cadence app show` lists the slots",
        ));
    }
    model::check_key(project_key)?;
    let _ = app_dir(&pm.dir, project_key, name)?;
    let manifest = read_manifest(&pm.dir, project_key, name)?;
    let mut record = read_record(&pm.dir, project_key, name)?;
    let known = known_connections(state_dir);
    let mut warnings = Vec::new();
    for binding in bindings {
        let Some((slot, conn)) = binding.split_once('=') else {
            return Err(Error::rejected(format!(
                "app set takes <slot>=<connection> — got '{binding}'"
            )));
        };
        check_name(slot, "connection slot")?;
        if !manifest.connections.iter().any(|s| s == slot) {
            return Err(Error::rejected(format!(
                "app '{name}' declares no slot '{slot}' — `needs.connections` has: {}",
                if manifest.connections.is_empty() {
                    "none".to_string()
                } else {
                    manifest.connections.join(", ")
                }
            )));
        }
        if conn.is_empty() {
            record.bindings.insert(slot.to_string(), None);
            continue;
        }
        crate::proto::identifier(conn, "connection name")?;
        if let Some(known) = &known {
            if !known.contains(conn) {
                warnings.push(format!(
                    "connection '{conn}' is not registered — the daemon knows: {}; \
                     `cadence doctor` reports slots bound to connections that do \
                     not exist",
                    known.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        record
            .bindings
            .insert(slot.to_string(), Some(conn.to_string()));
    }
    let _lock = pm.lock()?;
    let _legacy = pm.begin_legacy_write()?;
    let record_path = record_file(&pm.dir, project_key, name)?;
    write_record(&record_path, &record)?;
    let foreign = write::commit(
        pm,
        &[record_path],
        &format!("{project_key}/{DIR}/{name}: connection bindings set"),
        &[],
        actor,
    )?;
    let digest = digest(&pm.dir, project_key, name)?;
    let approvals = fetch_approvals(state_dir);
    let effective: Map<String, Value> = manifest
        .connections
        .iter()
        .map(|slot| {
            (
                slot.clone(),
                record.binding(slot).map_or(Value::Null, |c| json!(c)),
            )
        })
        .collect();
    let mut out = json!({
        "project": project_key,
        "name": name,
        "bindings": effective,
        "digest": digest,
        "approved": approved(
            project_key,
            name,
            &digest,
            &record.install_id,
            approvals.as_ref(),
        ),
        "committed": true,
        "warnings": warnings,
    });
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// `cadence app set-team <app> --role <input>=<agent> … --project <key>`
/// (CAD-577) — record the app's default team: one agent alias per
/// workflow input role. The team lives with the install record and is
/// NOT part of the gate digest, so setting it never re-requires
/// approval. Each role must be an input some workflow's step names as
/// its agent (the team inputs the drawer fills); each agent must be a
/// registered alias. An empty `--role <input>=` clears that role.
/// Operator-only at the daemon (the board relays it through
/// `operator_connection`); this function itself is the write.
pub fn set_team(
    pm: &Pm,
    project_key: &str,
    name: &str,
    roles: &[String],
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    if roles.is_empty() {
        return Err(Error::rejected(
            "app set-team needs <input>=<agent> — `cadence app show` lists the roles",
        ));
    }
    model::check_key(project_key)?;
    let _ = app_dir(&pm.dir, project_key, name)?;
    let mut record = read_record(&pm.dir, project_key, name)?;
    let roles_declared = team_roles(&pm.dir, project_key, name)?;
    let known = daemon_aliases(state_dir);
    let known: Option<HashSet<String>> = if known.is_empty() {
        None
    } else {
        Some(known.into_iter().collect())
    };
    for role in roles {
        let Some((input, agent)) = role.split_once('=') else {
            return Err(Error::rejected(format!(
                "app set-team takes <input>=<agent> — got '{role}'"
            )));
        };
        if !roles_declared.iter().any(|r| r == input) {
            return Err(Error::rejected(format!(
                "app '{name}' has no team role '{input}' — its workflows name: {}",
                if roles_declared.is_empty() {
                    "none".to_string()
                } else {
                    roles_declared.join(", ")
                }
            )));
        }
        if agent.is_empty() {
            record.team.remove(input);
            continue;
        }
        crate::proto::identifier(agent, "agent alias")?;
        if let Some(known) = &known {
            if !known.contains(agent) {
                return Err(Error::rejected(format!(
                    "agent '{agent}' is not registered — `cadence agent list` lists them"
                )));
            }
        }
        record.team.insert(input.to_string(), agent.to_string());
    }
    let _lock = pm.lock()?;
    let _legacy = pm.begin_legacy_write()?;
    let record_path = record_file(&pm.dir, project_key, name)?;
    write_record(&record_path, &record)?;
    let foreign = write::commit(
        pm,
        &[record_path],
        &format!("{project_key}/{DIR}/{name}: default team set"),
        &[],
        actor,
    )?;
    let mut out = json!({
        "project": project_key,
        "name": name,
        "team": record.team,
        "committed": true,
    });
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// The team roles an app's workflows declare: every input some step
/// names as its `agent:` (the same set the board's drawer fills by
/// role). Sorted, deterministic. Public so the daemon's "Add worker"
/// can check the role it is asked to fill (CAD-577).
pub fn team_roles(pm_dir: &Path, project_key: &str, name: &str) -> Result<Vec<String>> {
    let dir = app_dir(pm_dir, project_key, name)?;
    let mut roles: Vec<String> = Vec::new();
    for (rel, path) in bundle_files(&dir)? {
        let Some(_wf) = rel
            .strip_prefix("workflows/")
            .and_then(|n| n.strip_suffix(".md"))
        else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let Ok(tpl) = workflow::parse_template(&text) else {
            continue;
        };
        let declared: HashSet<&String> = tpl.inputs.keys().collect();
        let (_, body) = parse::split_front(&text).unwrap_or(("", ""));
        for meta in workflow::ticket_meta(body).unwrap_or_default() {
            for (key, value) in meta {
                if key != "agent" {
                    continue;
                }
                // The role is the input name whether the step names it
                // literally or as `{{role}}`.
                let role = value
                    .trim()
                    .strip_prefix("{{")
                    .and_then(|v| v.strip_suffix("}}"))
                    .map(str::trim)
                    .unwrap_or(value.trim());
                if declared.contains(&role.to_string()) && !roles.contains(&role.to_string()) {
                    roles.push(role.to_string());
                }
            }
        }
    }
    roles.sort();
    Ok(roles)
}

/// One derived grant the app's approval records: `agent` may call
/// `platform`/`account` at `scopes` (CAD-577). The scope is the slot
/// name — the local adapter's `publish` tool declares exactly that
/// scope — so a grant can never exceed what a step declared.
#[derive(Clone, Debug, PartialEq)]
pub struct DerivedGrant {
    pub agent: String,
    pub platform: String,
    pub account: String,
    pub scopes: Vec<String>,
}

/// The grants an app's approval derives (CAD-577): for every workflow
/// step that declares `uses: <slot>` and an `agent: {{role}}`, the
/// agent the app's default team assigns to that role gets the slot's
/// scope on the slot's bound connection. The operator sets the team
/// (an operator-only write), so an agent can never widen its own grant
/// by naming itself at propose. A slot with no effective binding, or a
/// role with no team entry, derives nothing — a grant never names a
/// connection the operator did not choose.
pub fn derive_grants(pm_dir: &Path, project_key: &str, name: &str) -> Result<Vec<DerivedGrant>> {
    let dir = app_dir(pm_dir, project_key, name)?;
    let record = read_record(pm_dir, project_key, name)?;
    let manifest = read_manifest(pm_dir, project_key, name)?;
    let mut out: Vec<DerivedGrant> = Vec::new();
    for (rel, path) in bundle_files(&dir)? {
        if rel
            .strip_prefix("workflows/")
            .and_then(|n| n.strip_suffix(".md"))
            .is_none()
        {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let Ok((_, body)) = parse::split_front(&text) else {
            continue;
        };
        let Ok(metas) = workflow::ticket_meta(body) else {
            continue;
        };
        for meta in metas {
            let mut role: Option<String> = None;
            let mut slots: Vec<String> = Vec::new();
            for (key, value) in meta {
                match key.as_str() {
                    "agent" => {
                        // The role placeholder `{{role}}` names an input;
                        // a literal alias is the app's own fixed choice.
                        let inner = value
                            .trim()
                            .strip_prefix("{{")
                            .and_then(|v| v.strip_suffix("}}"))
                            .map(str::trim)
                            .map(str::to_string);
                        role = inner.or(Some(value));
                    }
                    "uses" => {
                        for tok in value
                            .trim_start_matches('[')
                            .trim_end_matches(']')
                            .split(',')
                            .map(str::trim)
                            .filter(|t| !t.is_empty())
                        {
                            slots.push(tok.to_string());
                        }
                    }
                    _ => {}
                }
            }
            let Some(role) = role else { continue };
            // The agent the team assigns to this role — the operator's
            // choice, never a proposer input. A literal alias the
            // template hard-codes is used as-is (the app author's own
            // reviewed choice).
            let agent = if workflow::parse_template(&text)
                .ok()
                .is_some_and(|t| t.inputs.contains_key(&role))
            {
                match record.team.get(&role) {
                    Some(a) => a.clone(),
                    None => continue,
                }
            } else {
                role.clone()
            };
            for slot in &slots {
                if !manifest.connections.iter().any(|s| s == slot) {
                    continue;
                }
                let Some(conn) = record.binding(slot) else {
                    continue;
                };
                // CAD-577 derives grants for the built-in `local`
                // connection (account `local`). The generic
                // connection→account mapping lands with CAD-585; until
                // then a slot bound to any other connection derives no
                // grant rather than guessing an account.
                if conn != crate::platform::local::PLATFORM {
                    continue;
                }
                match out
                    .iter_mut()
                    .find(|g| g.agent == agent && g.platform == conn)
                {
                    Some(g) => {
                        if !g.scopes.contains(slot) {
                            g.scopes.push(slot.clone());
                        }
                    }
                    None => out.push(DerivedGrant {
                        agent: agent.clone(),
                        platform: conn.to_string(),
                        account: crate::platform::BUILTIN_LOCAL_ACCOUNT.to_string(),
                        scopes: vec![slot.clone()],
                    }),
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.agent, &a.platform).cmp(&(&b.agent, &b.platform)));
    for g in &mut out {
        g.scopes.sort();
        g.scopes.dedup();
    }
    Ok(out)
}

/// The connection names the daemon registers, best-effort — `local` is
/// always in the set (the built-in contract, CAD-546 or not). `None`
/// when the daemon is unreachable; callers then report "not verified"
/// rather than "unknown".
fn known_connections(state_dir: &Path) -> Option<HashSet<String>> {
    let v = crate::client::rpc(state_dir, "daemon_info", json!({})).ok()?;
    let mut set: HashSet<String> = v["connections"]
        .as_array()?
        .iter()
        .filter_map(|c| c.as_str().map(str::to_string))
        .collect();
    set.insert(LOCAL_CONNECTION.to_string());
    Some(set)
}

/// The manifest of an installed app (`apps/<name>/app.md`).
pub fn read_manifest(pm_dir: &Path, project: &str, name: &str) -> Result<Manifest> {
    let file = app_dir(pm_dir, project, name)?.join(MANIFEST);
    if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            file.display()
        )));
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))?;
    parse_manifest(&text)
}

/// The daemon's registered agent aliases, best-effort — install/update
/// resolve `agent:` against them like `workflow add` does.
fn daemon_aliases(state_dir: &Path) -> Vec<String> {
    crate::client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|v| v["agents"].as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|a| a["alias"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Every installed app of `project_key` (or all projects), one row per
/// `apps/<name>/` dir — plus one `error` row per stray entry (a symlink,
/// an orphan `<name>.yaml` with no folder, a folder with no record).
pub fn ls(pm: &Pm, project_key: Option<&str>, state_dir: &Path) -> Result<Value> {
    let projects = project::list(&pm.dir)?;
    let projects: Vec<&project::Project> = match project_key {
        Some(key) => {
            let found: Vec<&project::Project> = projects.iter().filter(|p| p.key == key).collect();
            if found.is_empty() {
                return Err(project::unknown_project(key, &pm.dir));
            }
            found
        }
        None => projects.iter().collect(),
    };
    let approvals = fetch_approvals(state_dir);
    let mut rows = Vec::new();
    for p in projects {
        let dir = pm.dir.join(&p.key).join(DIR);
        if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            rows.push(json!({
                "project": p.key, "name": null,
                "error": "apps/ is a symlink — the tracker never follows links",
            }));
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut seen_records: HashSet<String> = HashSet::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                rows.push(json!({
                    "project": p.key, "name": name,
                    "error": "symlink — the tracker never follows links",
                }));
                continue;
            }
            if ft.is_dir() {
                rows.push(describe(&pm.dir, &p.key, &name, approvals.as_ref()));
                continue;
            }
            if let Some(stem) = name.strip_suffix(".yaml") {
                seen_records.insert(stem.to_string());
                continue; // the record's own row is the app dir's
            }
            rows.push(json!({
                "project": p.key, "name": name,
                "error": "not an app — apps/ holds <name>/ folders and <name>.yaml records",
            }));
        }
        // A record without its folder — a half-removed app.
        for stem in seen_records {
            if !dir.join(&stem).is_dir() {
                rows.push(json!({
                    "project": p.key, "name": stem,
                    "error": "install record without its app folder — remove \
                              the stray file or reinstall",
                }));
            }
        }
    }
    rows.sort_by(|a, b| {
        (
            a["project"].as_str().unwrap_or(""),
            a["name"].as_str().unwrap_or(""),
        )
            .cmp(&(
                b["project"].as_str().unwrap_or(""),
                b["name"].as_str().unwrap_or(""),
            ))
    });
    Ok(json!({"apps": rows, "count": rows.len()}))
}

/// One installed app's summary row — used by `ls` and the board list.
fn describe(
    pm_dir: &Path,
    project: &str,
    name: &str,
    approvals: Option<&Map<String, Value>>,
) -> Value {
    let mut row = json!({"project": project, "name": name});
    let record = match read_record(pm_dir, project, name) {
        Ok(record) => record,
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    let manifest = match read_manifest(pm_dir, project, name) {
        Ok(manifest) => manifest,
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    let workflows: Vec<String> = match bundle_files(&app_dir_or(pm_dir, project, name)) {
        Ok(files) => files
            .iter()
            .filter_map(|(rel, _)| {
                rel.strip_prefix("workflows/")
                    .and_then(|n| n.strip_suffix(".md"))
                    .map(str::to_string)
            })
            .collect(),
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    row["title"] = json!(manifest.title);
    row["version"] = json!(manifest.version);
    row["summary"] = json!(manifest.summary);
    row["workflows"] = json!(workflows);
    // The card's primary action (CAD-563 r2): the app's first workflow
    // (sorted, so the choice is deterministic) and the human label the
    // board names the action with.
    let mut names = workflows.clone();
    names.sort();
    if let Some(first) = names.first() {
        let label = std::fs::read_to_string(
            app_dir_or(pm_dir, project, name)
                .join("workflows")
                .join(format!("{first}.md")),
        )
        .ok()
        .and_then(|text| workflow::parse_template(&text).ok())
        .and_then(|tpl| tpl.label);
        row["primary"] = json!({"workflow": first, "label": label});
    }
    row["connections"] = json!(manifest
        .connections
        .iter()
        .map(|slot| json!({
            "slot": slot,
            "bound": record.binding(slot),
        }))
        .collect::<Vec<_>>());
    match digest(pm_dir, project, name) {
        Ok(d) => {
            row["digest"] = json!(d);
            row["approved"] = match approvals {
                Some(a) => json!(approved(project, name, &d, &record.install_id, Some(a))),
                None => json!("unknown — daemon unreachable"),
            };
            row["approval"] = json!(approval_state(
                approvals,
                &approval_key(project, name),
                &d,
                &record.install_id,
            ));
        }
        Err(e) => row["error"] = json!(e.to_string()),
    }
    row["source"] = serde_json::to_value(&record.source).unwrap_or(Value::Null);
    row["team"] = json!(record.team);
    row["installed_at"] = json!(record.installed_at);
    row["installed_by"] = json!(record.installed_by);
    row["updated_at"] = json!(record.updated_at);
    row
}

/// `apps/<name>/` without the "must be installed" wording — for
/// internal walkers that already know it exists.
fn app_dir_or(pm_dir: &Path, project: &str, name: &str) -> PathBuf {
    pm_dir.join(project).join(DIR).join(name)
}

/// `cadence app show <app> --project <key>` — the manifest, the guide,
/// each workflow's summary (the same detail `workflow show` gives),
/// declared slots with their effective bindings, the install record,
/// the digest and the approval state.
pub fn show(pm: &Pm, project_key: &str, name: &str, state_dir: &Path) -> Result<Value> {
    model::check_key(project_key)?;
    if !project::list(&pm.dir)?.iter().any(|p| p.key == project_key) {
        return Err(project::unknown_project(project_key, &pm.dir));
    }
    let mut out = describe(
        &pm.dir,
        project_key,
        name,
        fetch_approvals(state_dir).as_ref(),
    );
    if out.get("error").is_some() {
        return Ok(out);
    }
    let dir = app_dir(&pm.dir, project_key, name)?;
    let manifest = read_manifest(&pm.dir, project_key, name)?;
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) =
        workflow::known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let mut workflows = Vec::new();
    let mut rubrics = Vec::new();
    for (rel, path) in bundle_files(&dir)? {
        if let Some(rb) = rel
            .strip_prefix("rubrics/")
            .and_then(|n| n.strip_suffix(".md"))
        {
            // A file verified real at scan could still be swapped for a
            // link before the read — re-check the leaf, like
            // `read_workflow` and `board_rows` do.
            if path
                .symlink_metadata()
                .map(|m| !m.is_file())
                .unwrap_or(true)
            {
                continue;
            }
            rubrics.push(json!({
                "name": rb,
                "body": std::fs::read_to_string(&path).unwrap_or_default(),
            }));
            continue;
        }
        let Some(wf) = rel
            .strip_prefix("workflows/")
            .and_then(|n| n.strip_suffix(".md"))
            .map(str::to_string)
        else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        // The installed bundle's own verdict: a `skill:` tag is admissible
        // here, and `validate` proved it resolves.
        let (errors, notes, doc) = workflow::check_installed_text(&text, &agents, &agent_sources);
        let tpl = workflow::parse_template(&text).ok();
        workflows.push(json!({
            "name": format!("{name}/{wf}"),
            "ok": errors.is_empty(),
            "errors": errors,
            "notes": notes,
            "title": doc.as_ref().map(|d| d.title.clone()),
            // The workflow's human label (`label:`, wording) — the
            // board's primary action names it; the app's title when the
            // workflow never declared one.
            "label": tpl.as_ref().and_then(|t| t.label.clone()),
            "tickets": doc.as_ref().map(|d| d.tickets.len()),
            "inputs": tpl.as_ref().map(workflow::inputs_json),
            // The steps, in order, from the canonical render: each
            // title (input names stand in for values) and the agent
            // expression — an input name when the ticket's `agent:` is
            // exactly `{{input}}`, which is how the board maps a run's
            // owners back to the team inputs.
            "steps": doc.as_ref().map(|d| d.tickets.iter().map(|t| json!({
                "title": t.title,
                "agent": t.agent,
                "size": t.size,
            })).collect::<Vec<_>>()),
            "distinct": tpl.as_ref().map(|t| t.distinct.clone()),
            "uses": workflow_slots(&text).unwrap_or_default(),
        }));
    }
    let record = read_record(&pm.dir, project_key, name)?;
    out["summary"] = json!(manifest.summary);
    out["guide"] = json!(manifest.guide);
    out["workflows"] = json!(workflows);
    out["rubrics"] = json!(rubrics);
    out["record"] = serde_json::to_value(&record).unwrap_or(Value::Null);
    Ok(out)
}

/// `plan propose --workflow <app>/<wf>` resolution — the daemon's read:
/// the app must be installed (record included), its current digest must
/// match the operator's recorded approval (`app_unapproved` otherwise),
/// then the workflow renders exactly as a stored one would. The app's
/// saved default team (CAD-577) fills any team role the proposal left
/// unset — the operator set it, so a fresh install runs with only a
/// topic; an explicit input still wins.
pub fn plan_text(
    pm_dir: &Path,
    app_approvals: &std::collections::HashMap<String, Value>,
    project: &str,
    app: &str,
    wf: &str,
    provided: &BTreeMap<String, String>,
) -> Result<String> {
    let _ = app_dir(pm_dir, project, app)?;
    let record = read_record(pm_dir, project, app)?;
    let text = read_workflow(pm_dir, project, app, wf)?;
    // CAD-1867: a `skill:` directive is resolved only by an app RUN, which
    // freezes the package into its snapshot. A plan proposal renders the
    // workflow into tracker tickets — it freezes nothing — so the directive
    // refuses here instead of being dropped into prose.
    workflow::refuse_skill_directive(&text)?;
    // The digest covers THIS buffer for the rendered workflow — the
    // bytes the operator approved are the bytes rendered, even if a
    // co-host writer flips the file after this one read (N2).
    let rel = format!("workflows/{wf}.md");
    let digest = digest_over(pm_dir, project, app, &[(rel.as_str(), text.as_str())])?;
    let ok = app_approvals
        .get(&approval_key(project, app))
        .is_some_and(|p| approval_binds(p, digest.as_str(), &record.install_id));
    if !ok {
        return Err(Error::invalid(
            "app_unapproved",
            format!(
                "app '{app}' in {project} is not approved for its current structure \
                 ({digest}) — a slot, binding, guide, rubric, template or workflow \
                 gate change resets it; the operator re-approves with `cadence app \
                 approve {app} --project {project}`"
            ),
        ));
    }
    // The saved default team fills any team role THIS workflow declares
    // and the proposal left unset (CAD-577). The team record is the
    // union across the app's workflows; filling a role another workflow
    // declares makes `render` refuse `unknown input`. An explicit input
    // still wins: the caller named that agent.
    let declared = workflow::parse_template(&text).ok();
    let mut inputs = provided.clone();
    for (role, agent) in &record.team {
        if declared
            .as_ref()
            .is_some_and(|tpl| tpl.inputs.contains_key(role))
        {
            inputs.entry(role.clone()).or_insert_with(|| agent.clone());
        }
    }
    workflow::render(&text, &inputs)
}

/// `app approve`'s view of the INSTALLED folder — the same strict scan
/// and checks a fresh source passes, so a hand edit after install
/// cannot smuggle content past the operator's review.
pub fn check_installed(
    pm_dir: &Path,
    project: &str,
    name: &str,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Vec<String>> {
    let dir = app_dir(pm_dir, project, name)?;
    let _ = read_record(pm_dir, project, name)?;
    Ok(validate(&dir, agents, agent_sources)?.notes)
}

/// The board's New-run rows for `project`'s app workflows — one per
/// `apps/<app>/workflows/<wf>.md`, named `<app>/<wf>`, carrying the
/// APP's approval state (approval is whole-app; there is no
/// per-workflow approval). An app that fails its installed checks lists
/// its workflows with the error attached — the board shows why it
/// cannot run rather than hiding it.
pub fn board_rows(pm_dir: &Path, project: &str, state_dir: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    let dir = pm_dir.join(project).join(DIR);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return rows;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return rows;
    };
    let approvals = fetch_approvals(state_dir);
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) = workflow::known_agents(pm_dir, Some(project), &daemon_agents);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(ft) = e.file_type() else {
            continue;
        };
        if !ft.is_dir() || !model::valid_tag(&name) {
            continue;
        }
        let checked = check_installed(pm_dir, project, &name, &agents, &agent_sources)
            .map_err(|e| e.to_string());
        let app_digest = digest(pm_dir, project, &name);
        let install_id = current_install_id(pm_dir, project, &name);
        let approved = match &app_digest {
            Ok(d) => match &approvals {
                Some(a) => json!(approved(project, &name, d, &install_id, Some(a))),
                None => json!("unknown — daemon unreachable"),
            },
            Err(_) => Value::Null,
        };
        // The listing goes through bundle_files — the same no-links walk
        // install and approve enforce — so a symlinked workflows/ dir (or
        // any planted link) becomes the app's error row instead of
        // leaking the foreign dir's file names onto the board (N1).
        match bundle_files(&e.path()) {
            Ok(files) => {
                for (rel, path) in &files {
                    let Some(stem) = rel
                        .strip_prefix("workflows/")
                        .and_then(|n| n.strip_suffix(".md"))
                    else {
                        continue;
                    };
                    // A file verified real at scan could still be swapped
                    // for a link before the read — re-check the leaf.
                    if path
                        .symlink_metadata()
                        .map(|m| !m.is_file())
                        .unwrap_or(true)
                    {
                        continue;
                    }
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    let (_, _, doc) =
                        workflow::check_installed_text(&text, &agents, &agent_sources);
                    let tpl = workflow::parse_template(&text).ok();
                    let mut row = json!({
                        "project": project,
                        "name": format!("{name}/{stem}"),
                        "app": name,
                        "title": doc.as_ref().map(|d| d.title.clone()),
                        "tickets": doc.map(|d| d.tickets.len()),
                        "inputs": tpl.as_ref().map(workflow::inputs_json),
                        "approved": approved,
                        "digest": app_digest.as_ref().ok(),
                    });
                    if let Err(why) = &checked {
                        row["error"] = json!(why);
                    }
                    rows.push(row);
                }
            }
            Err(why) => {
                // One row still names the broken app — the board shows
                // why it cannot run rather than hiding it.
                let mut row = json!({
                    "project": project,
                    "name": name,
                    "app": name,
                    "title": Value::Null,
                    "tickets": Value::Null,
                    "inputs": Value::Null,
                    "approved": approved,
                    "digest": app_digest.as_ref().ok(),
                });
                row["error"] = match &checked {
                    Err(checked_why) => json!(checked_why),
                    Ok(_) => json!(why.to_string()),
                };
                rows.push(row);
            }
        }
    }
    rows
}

/// `issue lint`'s view of `<pm>/<key>/apps/` — each `<name>/` is an app
/// folder with a parseable `app.md`, each `<name>.yaml` a well-formed
/// record that names it; anything else is a warning, a symlink an
/// error, like the workflows dir's own lint.
pub fn lint_dir(
    dir: &Path,
    project_key: &str,
    err: &mut dyn FnMut(String),
    warn: &mut dyn FnMut(String),
) {
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        err(format!(
            "{project_key}/{DIR}: symlink — the board never follows links"
        ));
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = Vec::new();
    let mut records: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            err(format!(
                "{project_key}/{DIR}/{name}: symlink — the board never follows links"
            ));
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if !model::valid_tag(&name) {
                warn(format!(
                    "{project_key}/{DIR}/{name}: not an app name — [a-z0-9-], ≤32"
                ));
                continue;
            }
            names.push(name);
            continue;
        }
        if let Some(stem) = name.strip_suffix(".yaml") {
            records.push(stem.to_string());
            continue;
        }
        warn(format!(
            "{project_key}/{DIR}/{name}: not an app — apps/ holds <name>/ folders \
             and <name>.yaml records"
        ));
    }
    for name in &names {
        let app_path = dir.join(name);
        let md = app_path.join(MANIFEST);
        if md.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            err(format!(
                "{project_key}/{DIR}/{name}/app.md: symlink — the board never follows links"
            ));
            continue;
        }
        match std::fs::read_to_string(&md) {
            Err(_) => err(format!("{project_key}/{DIR}/{name}: no app.md")),
            Ok(text) => match parse_manifest(&text) {
                Err(e) => err(format!("{project_key}/{DIR}/{name}/app.md: {e}")),
                Ok(m) if m.app != *name => warn(format!(
                    "{project_key}/{DIR}/{name}/app.md: declares app '{}' — the folder \
                     and record name it",
                    m.app
                )),
                Ok(_) => {}
            },
        }
        if !records.iter().any(|r| r == name) {
            warn(format!(
                "{project_key}/{DIR}/{name}: no install record {name}.yaml — it was \
                 not installed by `cadence app install`"
            ));
        }
    }
    for stem in &records {
        if names.iter().any(|n| n == stem) {
            continue;
        }
        warn(format!(
            "{project_key}/{DIR}/{stem}.yaml: install record without its app folder"
        ));
    }
}

/// `cadence doctor`'s app report — per installed app, every declared
/// slot's binding health: `unbound` (explicitly unbound), `unknown` when
/// the connection name isn't registered (`known` is the daemon's
/// connection set — `None` when unreachable, reported as
/// "unavailable", never as "unknown connection"), plus slots bound
/// that the manifest no longer declares.
pub fn doctor(pm_dir: &Path, known: Option<&HashSet<String>>) -> Value {
    let mut rows = Vec::new();
    let Ok(projects) = project::list(pm_dir) else {
        return json!({"apps": [], "note": "no readable PM projects"});
    };
    for p in projects {
        let apps_dir = pm_dir.join(&p.key).join(DIR);
        if apps_dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            rows.push(json!({
                "project": p.key, "app": null,
                "error": "apps/ is a symlink — never followed",
            }));
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&apps_dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) || !model::valid_tag(&name) {
                continue;
            }
            let (manifest, record) = match (
                read_manifest(pm_dir, &p.key, &name),
                read_record(pm_dir, &p.key, &name),
            ) {
                (Ok(m), Ok(r)) => (m, r),
                (a, b) => {
                    let why = a
                        .err()
                        .or(b.err())
                        .map(|e| e.to_string())
                        .unwrap_or_default();
                    rows.push(json!({"project": p.key, "app": name, "error": why}));
                    continue;
                }
            };
            let mut unbound = Vec::new();
            let mut unknown = Vec::new();
            let mut ok = Vec::new();
            for slot in &manifest.connections {
                match record.binding(slot) {
                    None => unbound.push(slot.clone()),
                    Some(conn) => match known {
                        None => ok.push(
                            json!({"slot": slot, "connection": conn, "verified": "unavailable"}),
                        ),
                        Some(known) if known.contains(conn) => {
                            ok.push(json!({"slot": slot, "connection": conn}))
                        }
                        Some(_) => unknown.push(json!({"slot": slot, "connection": conn})),
                    },
                }
            }
            let stray: Vec<&String> = record
                .bindings
                .keys()
                .filter(|k| !manifest.connections.iter().any(|s| s == *k))
                .collect();
            let mut row = json!({
                "project": p.key,
                "app": name,
                "slots_ok": ok,
                "unbound": unbound,
                "unknown_connection": unknown,
            });
            if !stray.is_empty() {
                row["stray_bindings"] = json!(stray);
            }
            if known.is_none() {
                row["connection_check"] = json!("unavailable — daemon unreachable");
            }
            rows.push(row);
        }
    }
    json!({"apps": rows, "count": rows.len()})
}

#[cfg(test)]
#[path = "app_nested_copy_acceptance.rs"]
mod nested_copy_acceptance;

#[cfg(test)]
mod tests {
    use super::*;

    /// Manifest admission retains supplied requirements and refuses malformed ones.
    #[test]
    fn manifest_wires_requires() {
        let legacy = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  connections: []\n---\n\nG\n";
        let m = parse_manifest(legacy).unwrap();
        assert!(!m.requires.declared(), "absent requires is legacy");

        let declared = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: {core: \">=0.0.0\", contracts: {app-chat: [1]}}\n---\n\nG\n";
        let m = parse_manifest(declared).unwrap();
        assert!(m.requires.declared());
        assert_eq!(m.requires.contracts["app-chat"], vec![1]);

        let malformed = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: {contracts: {app-chat: [0]}}\n---\n\nG\n";
        assert!(parse_manifest(malformed).is_err());
        let nonmap = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: ''\n---\n\nG\n";
        assert!(parse_manifest(nonmap).is_err());
        let bad_ver = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: {core: '>=1.0.0-alpha..1'}\n---\n\nG\n";
        assert!(parse_manifest(bad_ver).is_err());
        let bad_build = "---\napp: x\ntitle: t\nversion: '1'\nneeds:\n  requires: {core: '>=1.0.0+'}\n---\n\nG\n";
        assert!(parse_manifest(bad_build).is_err());
    }

    #[test]
    fn cad692_publication_declaration_is_provider_neutral_and_bounded() {
        let good = "---\napp: studio\ntitle: Studio\nversion: '1'\nneeds:\n  capabilities:\n    publication: {schema: 1, capability: text.publish, version: 1, action: publish, resource_kind: connection_account, effect: send}\n---\nGuide\n";
        let manifest = parse_manifest(good).unwrap();
        assert_eq!(
            manifest.capabilities["publication"].capability,
            "text.publish"
        );
        assert!(manifest.connections.is_empty());
        for bad in [
            good.replace("effect: send", "effect: send, tool: publish_app_text"),
            good.replace("effect: send", "effect: send, provider: local"),
            good.replace("version: 1, action:", "version: 2, action:"),
            good.replace(
                "capabilities:",
                "connections: [publication]\n  capabilities:",
            ),
            good.replace("effect: send", "effect: draft"),
        ] {
            assert!(parse_manifest(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cad632_read_and_draft_declarations_are_provider_neutral() {
        let app = "---\napp: studio\ntitle: Studio\nversion: '1'\nneeds:\n  capabilities:\n    source: {schema: 1, capability: social.read, version: 1, action: posts, resource_kind: connection_account, effect: read}\n    image: {schema: 1, capability: media.generate, version: 1, action: image, resource_kind: connection_account, effect: draft}\n---\nGuide\n";
        let parsed = parse_manifest(app).unwrap();
        assert_eq!(parsed.capabilities["source"].effect, "read");
        assert_eq!(parsed.capabilities["image"].effect, "draft");
        for bad in [
            app.replace("effect: read", "effect: send"),
            app.replace("social.read", "social.read;platform_call"),
            app.replace(
                "resource_kind: connection_account",
                "resource_kind: workspace",
            ),
        ] {
            assert!(parse_manifest(&bad).is_err(), "{bad}");
        }
    }

    const APP_MD: &str = "---\napp: content-studio\ntitle: Content studio\nversion: 0.1.0\n\
needs:\n  connections: [publish]\n---\n\n# Guide\n\nHow to run the studio.\n";

    #[test]
    fn manifest_parses_fields_and_slots() {
        let m = parse_manifest(APP_MD).unwrap();
        assert_eq!(m.app, "content-studio");
        assert_eq!(m.title, "Content studio");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.connections, vec!["publish".to_string()]);
        assert_eq!(m.summary, None, "no summary is fine");
        assert!(m.guide.contains("Guide"));
    }

    /// CAD-563: `summary:` is the optional one-line purpose the board
    /// shows — accepted and trimmed; a non-string or an oversize one
    /// refuses like every other manifest field.
    #[test]
    fn manifest_summary_is_optional_wording() {
        let with = APP_MD.replace(
            "version: 0.1.0",
            "version: 0.1.0\nsummary:  Get a post written, checked and published.  ",
        );
        let m = parse_manifest(&with).unwrap();
        assert_eq!(
            m.summary.as_deref(),
            Some("Get a post written, checked and published.")
        );
        // A blank summary reads as none — a `summary:` line left empty
        // is not a refusal.
        let blank = APP_MD.replace("version: 0.1.0", "version: 0.1.0\nsummary: '  '");
        assert_eq!(parse_manifest(&blank).unwrap().summary, None);
        for bad in [
            APP_MD.replace("version: 0.1.0", "version: 0.1.0\nsummary: [a, b]"),
            APP_MD.replace(
                "version: 0.1.0",
                &format!("version: 0.1.0\nsummary: '{}'", "x".repeat(161)),
            ),
        ] {
            assert!(parse_manifest(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn manifest_refuses_gated_and_unknown_keys() {
        for bad in [
            "---\napp: x\ntitle: t\nversion: v\nrecords: {}\n---\n\nbody\n",
            "---\napp: x\ntitle: t\nversion: v\nbogus: 1\n---\n\nbody\n",
            "---\napp: x\ntitle: t\nversion: v\nneeds: {other: [a]}\n---\n\nbody\n",
        ] {
            assert!(parse_manifest(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn manifest_requires_fields_and_good_names() {
        for bad in [
            "---\ntitle: t\nversion: v\n---\n\nbody\n",
            "---\napp: 'Bad Name'\ntitle: t\nversion: v\n---\n\nbody\n",
            "---\napp: x\ntitle: t\n---\n\nbody\n",
        ] {
            assert!(parse_manifest(bad).is_err(), "{bad}");
        }
    }

    /// Provider names alone cannot assign accounts or widen app grants.
    /// Capability matching remains a separately reviewed resolver.
    #[test]
    fn provider_binding_names_do_not_derive_unreviewed_app_grants() {
        let pm = tempfile::tempdir().unwrap();
        let apps = pm.path().join("demo").join("apps");
        let dir = apps.join("capability-demo");
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        std::fs::write(
            dir.join("app.md"),
            "---\napp: capability-demo\ntitle: Social\nversion: 0.1.0\n\
needs:\n  connections: [publish]\n---\n\n# Guide\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("workflows").join("post.md"),
            "---\ntitle: Post\ngoal: Publish\ninputs:\n  publisher: { ask: Who }\n---\n\n\
## Publish\nagent: {{publisher}}\nuses: publish\n\nStage it.\n",
        )
        .unwrap();
        std::fs::write(
            apps.join("capability-demo.yaml"),
            "schema: 1\napp: capability-demo\ninstall_id: inst-1\n\
source:\n  kind: path\n  path: /tmp/src\n\
bindings:\n  publish: agenticos\n\
team:\n  publisher: pub-1\n\
installed_at: '2026-09-27T00:00:00Z'\ninstalled_by: operator\n",
        )
        .unwrap();
        let grants = derive_grants(pm.path(), "demo", "capability-demo").unwrap();
        assert!(
            grants.is_empty(),
            "provider names alone must never derive automatic grants"
        );

        let yaml = apps.join("capability-demo.yaml");
        let text = std::fs::read_to_string(&yaml)
            .unwrap()
            .replace("agenticos", "other");
        std::fs::write(&yaml, text).unwrap();
        let grants = derive_grants(pm.path(), "demo", "capability-demo").unwrap();
        assert!(grants.is_empty(), "{grants:?}");
    }

    /// CAD-864: `needs.views` declares the one descriptor contract a
    /// bundle may carry; every other shape or value refuses, and the
    /// gated `ui`/`schema` keys still refuse at top level.
    #[test]
    fn manifest_views_declaration_is_exact() {
        let base = "---\napp: demo\ntitle: Demo\nversion: '1'\nneeds:\n  views:\n    contract: app-views/v1\n---\n\nGuide\n";
        let m = parse_manifest(base).unwrap();
        assert_eq!(m.view_contract.as_deref(), Some(app_view::CONTRACT));
        for bad in [
            // Unknown needs key still refuses.
            "---\napp: demo\ntitle: Demo\nversion: '1'\nneeds:\n  viewz: {}\n---\n\nGuide\n",
            // Wrong contract tag.
            "---\napp: demo\ntitle: Demo\nversion: '1'\nneeds:\n  views:\n    contract: app-views/v2\n---\n\nGuide\n",
            // views is not a bare scalar.
            "---\napp: demo\ntitle: Demo\nversion: '1'\nneeds:\n  views: app-views/v1\n---\n\nGuide\n",
            // Extra keys under needs.views refuse.
            "---\napp: demo\ntitle: Demo\nversion: '1'\nneeds:\n  views:\n    contract: app-views/v1\n    file: views/other.json\n---\n\nGuide\n",
            // Gated keys are still gated.
            "---\napp: demo\ntitle: Demo\nversion: '1'\nui: {}\n---\n\nGuide\n",
        ] {
            assert!(parse_manifest(bad).is_err(), "{bad}");
        }
        // No declaration → None.
        let none = parse_manifest(APP_MD).unwrap();
        assert_eq!(none.view_contract, None);
    }

    /// CAD-864: declaration and descriptor file pair up in the
    /// validator — either alone refuses, a mismatched `app` refuses.
    #[test]
    fn descriptor_pairs_with_its_declaration() {
        let agents = HashSet::new();
        let descriptor = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("contracts/app-views/v1/examples/crm.json"),
        )
        .unwrap()
        .replace("\"app\": \"crm\"", "\"app\": \"demo\"");
        let manifest = "---\napp: demo\ntitle: Demo\nversion: '1'\nneeds:\n  views:\n    contract: app-views/v1\n---\n\nGuide\n";
        let workflow_md = "---\ntitle: Do\ngoal: g\n---\n\n## Step\nagent: a\n\nDo.\n\n### Acceptance\n- [ ] done\n";
        let files = vec![
            ("app.md".to_string(), manifest.to_string()),
            ("workflows/do.md".to_string(), workflow_md.to_string()),
            (app_view::REL_PATH.to_string(), descriptor.clone()),
        ];
        validate_texts(files.clone(), &agents, &[]).unwrap();
        // Declaration without the file refuses.
        let no_file: Vec<_> = files
            .iter()
            .filter(|(rel, _)| rel != app_view::REL_PATH)
            .cloned()
            .collect();
        assert!(validate_texts(no_file, &agents, &[]).is_err());
        // File without the declaration refuses.
        let no_decl = vec![
            ("app.md".to_string(), APP_MD.to_string()),
            ("workflows/do.md".to_string(), workflow_md.to_string()),
            (app_view::REL_PATH.to_string(), descriptor.clone()),
        ];
        assert!(validate_texts(no_decl, &agents, &[]).is_err());
        // Descriptor `app` must be the manifest's app.
        let other = descriptor.replace("\"app\": \"demo\"", "\"app\": \"other\"");
        let mismatched = vec![
            ("app.md".to_string(), manifest.to_string()),
            ("workflows/do.md".to_string(), workflow_md.to_string()),
            (app_view::REL_PATH.to_string(), other),
        ];
        assert!(validate_texts(mismatched, &agents, &[]).is_err());
        // Malformed descriptor refuses.
        let bad = vec![
            ("app.md".to_string(), manifest.to_string()),
            ("workflows/do.md".to_string(), workflow_md.to_string()),
            (app_view::REL_PATH.to_string(), "{}".to_string()),
        ];
        assert!(validate_texts(bad, &agents, &[]).is_err());
    }

    #[test]
    fn split_ref_accepts_only_app_slash_workflow() {
        assert_eq!(split_ref("app/wf"), Some(("app", "wf")));
        assert_eq!(split_ref("wf"), None);
        assert_eq!(split_ref("a/b/c"), None);
        assert_eq!(split_ref("a/"), None);
        assert_eq!(split_ref("/b"), None);
        assert_eq!(split_ref("A/b"), None);
    }
    /// CAD-1177: a workflow-free app installs only when it is a genuine
    /// standalone-tools plugin — an integrity-checked `app-screens/v2`
    /// screen whose `tools` all resolve to declared read/draft
    /// capability slots. An empty app, a failing-integrity screen, or a
    /// tool naming an undeclared/send slot cannot substitute for
    /// workflows and still refuses.
    #[test]
    fn cad1177_workflow_free_app_installs_only_with_a_real_v2_tools_screen() {
        let agents = HashSet::new();
        let js = "console.log('x')";
        let sha = |b: &str| format!("sha256:{:x}", Sha256::digest(b.as_bytes()));
        let manifest = "---\napp: studio\ntitle: Studio\nversion: '1'\nneeds:\n  capabilities:\n    source: {schema: 1, capability: social.read, version: 1, action: posts, resource_kind: connection_account, effect: read}\n---\nGuide\n";
        let screen = |tools: serde_json::Value| {
            serde_json::json!({
                "contract":"app-screens/v2","app":"studio","entry":"client.js",
                "host_contract":"screen-actions.v1","may":["tools.invoke"],
                "tools":tools,
                "assets":[{"name":"client.js","media_type":"text/javascript","sha256":sha(js),"size":js.len()}],
                "provenance":{"source_digest":sha("s"),"sdk_digest":sha("k"),"toolchain_digest":sha("t")}
            }).to_string()
        };
        let files = |decl: String| {
            vec![
                ("app.md".to_string(), manifest.to_string()),
                ("screens/composer/screens.json".to_string(), decl),
                ("screens/composer/client.js".to_string(), js.to_string()),
            ]
        };
        // A valid tools screen mapped to the declared `source` slot installs.
        validate_texts(
            files(screen(serde_json::json!({"instagram.read":"source"}))),
            &agents,
            &[],
        )
        .unwrap();
        // Empty app (no workflows, no qualifying screen) refuses.
        let empty = vec![("app.md".to_string(), manifest.to_string())];
        assert!(validate_texts(empty, &agents, &[]).is_err());
        // A v1 (read-only) screen is not a tools plugin.
        let mut v1: serde_json::Value =
            serde_json::from_str(&screen(serde_json::json!({"a.b":"source"}))).unwrap();
        v1["contract"] = serde_json::json!("app-screens/v1");
        v1.as_object_mut().unwrap().remove("tools");
        v1.as_object_mut().unwrap().remove("host_contract");
        v1["may"] = serde_json::json!([]);
        assert!(validate_texts(files(v1.to_string()), &agents, &[]).is_err());
        // A tool naming an undeclared slot refuses.
        assert!(validate_texts(
            files(screen(serde_json::json!({"a.b":"nosuchslot"}))),
            &agents,
            &[]
        )
        .is_err());
        // A tool resolving to a send-effect slot cannot stand alone.
        let send_manifest = manifest.replace("effect: read", "effect: send");
        let send_files = vec![
            ("app.md".to_string(), send_manifest),
            (
                "screens/composer/screens.json".to_string(),
                screen(serde_json::json!({"a.b":"source"})),
            ),
            ("screens/composer/client.js".to_string(), js.to_string()),
        ];
        assert!(validate_texts(send_files, &agents, &[]).is_err());
        // A broken-integrity screen (tampered client.js) refuses.
        let mut tampered = files(screen(serde_json::json!({"a.b":"source"})));
        tampered[2].1 = "tampered()".to_string();
        assert!(validate_texts(tampered, &agents, &[]).is_err());
    }

    /// CAD-1177: the `ig-tools-fixture` bundle ships NO `workflows/` —
    /// it is the real on-disk tools-only app QA mounts and installs
    /// against. This proves the shipped fixture validates (the
    /// acceptance assertions about it belong to QA).
    #[test]
    fn cad1177_ig_tools_fixture_is_a_valid_workflow_free_app() {
        let agents = HashSet::new();
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apps/ig-tools-fixture");
        let validated = validate(&dir, &agents, &[]).unwrap();
        assert_eq!(validated.manifest.app, "ig-tools-fixture");
        // No workflows members — the screen is the unit of work.
        assert!(validated
            .files
            .iter()
            .all(|(rel, _)| !rel.starts_with("workflows/")));
        // Its v2 screen integrity-checks and maps the alias to `source`.
        let map: BTreeMap<String, String> = validated.files.iter().cloned().collect();
        let pkg = crate::issue::app_screen_pkg::extract(&map, "feed").unwrap();
        assert_eq!(pkg.contract, "app-screens/v2");
        assert_eq!(pkg.tools["instagram.read"], "source");
    }

    /// CAD-1867: a bundle carrying one complete skill package, as the text map
    /// install and run creation both validate. `declared` is the manifest's
    /// `files`; `carried` is what the bundle actually holds.
    fn skill_bundle(
        declared: &[&str],
        carried: &[(&str, &str)],
        workflow: &str,
    ) -> BTreeMap<String, String> {
        let manifest = serde_json::json!({
            "schema": 1,
            "name": "meeting-writer",
            "files": declared,
        });
        let mut files: BTreeMap<String, String> = BTreeMap::new();
        files.insert(
            MANIFEST.to_string(),
            "---\napp: skill-app\ntitle: Skill app\nversion: '1'\nneeds:\n  \
             connections: []\n---\n\nGuide.\n"
                .to_string(),
        );
        files.insert("workflows/do.md".to_string(), workflow.to_string());
        files.insert(
            format!("{SKILL_DIR}/meeting-writer/{SKILL_MANIFEST}"),
            manifest.to_string(),
        );
        for (member, text) in carried {
            files.insert(
                format!("{SKILL_DIR}/meeting-writer/{member}"),
                text.to_string(),
            );
        }
        files
    }

    /// The storage guard workflow with a `skill:` line appended as the step's
    /// last metadata line — the only position the plan parser can read.
    fn skill_workflow(skill_line: &str) -> String {
        SKILL_BASE_WORKFLOW.replace(
            "agent: dev-1\nsize: S\n",
            &format!("agent: dev-1\nsize: S\n{skill_line}"),
        )
    }

    /// A minimal valid workflow the install gate accepts.
    const SKILL_BASE_WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n---\n\n\
Why.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n";

    fn skill_texts(files: &BTreeMap<String, String>) -> Vec<(String, String)> {
        files.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    fn install_error(files: &BTreeMap<String, String>) -> String {
        match validate_texts(skill_texts(files), &HashSet::new(), &[]) {
            Ok(_) => panic!("install accepts the bundle"),
            Err(error) => error.to_string(),
        }
    }

    const SKILL_MD: &str = "Read references/style.md, then run ./scripts/build.py.\n";
    const STYLE_MD: &str = "Short sentences.\n";
    const BUILD_PY: &str = "print('ok')\n";

    /// The complete package installs and freezes every declared member
    /// byte-exactly, with the manifest's own digest.
    #[test]
    fn cad1867_a_complete_skill_package_installs_and_freezes_its_bytes() {
        let files = skill_bundle(
            &[SKILL_ENTRY, "references/style.md", "scripts/build.py"],
            &[
                (SKILL_ENTRY, SKILL_MD),
                ("references/style.md", STYLE_MD),
                ("scripts/build.py", BUILD_PY),
            ],
            &skill_workflow("skill: meeting-writer\n"),
        );
        let validated = validate_texts(skill_texts(&files), &HashSet::new(), &[]).unwrap();
        assert_eq!(validated.manifest.app, "skill-app");

        let package = skill_package(&files, "meeting-writer").unwrap();
        assert_eq!(package.name, "meeting-writer");
        assert_eq!(
            package
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>(),
            vec![SKILL_ENTRY, "references/style.md", "scripts/build.py"]
        );
        for file in &package.files {
            let exact = &files[&format!("{SKILL_DIR}/meeting-writer/{}", file.path)];
            assert_eq!(&file.text, exact, "{} froze different bytes", file.path);
            assert_eq!(
                file.sha256,
                format!("sha256:{}", sha256_hex(exact.as_bytes()))
            );
        }
        let manifest_path = format!("{SKILL_DIR}/meeting-writer/{SKILL_MANIFEST}");
        assert_eq!(
            package.manifest_sha256,
            format!("sha256:{}", sha256_hex(files[&manifest_path].as_bytes()))
        );
        assert!(package.digest.starts_with("sha256:"));
        // Deterministic: the same bytes freeze to the same package.
        assert_eq!(package, skill_package(&files, "meeting-writer").unwrap());
    }

    /// A manifest that omits a member its own declared text references is
    /// refused at install, and the SAME validator refuses it again at run
    /// creation — one function, so the two cannot disagree.
    #[test]
    fn cad1867_an_undeclared_reference_refuses_at_install_and_at_run_creation() {
        // SKILL.md names `references/style.md`; the manifest omits it and the
        // bundle does not carry it either, so only the reference scan catches
        // it.
        let files = skill_bundle(
            &[SKILL_ENTRY, "scripts/build.py"],
            &[(SKILL_ENTRY, SKILL_MD), ("scripts/build.py", BUILD_PY)],
            &skill_workflow("skill: meeting-writer\n"),
        );
        let install = install_error(&files);
        assert!(install.contains("references/style.md"), "{install}");
        let run = skill_package(&files, "meeting-writer")
            .expect_err("run creation refuses")
            .to_string();
        assert!(run.contains("references/style.md"), "{run}");
    }

    /// The declared set must equal the present set in both directions, each
    /// mismatch naming the path.
    #[test]
    fn cad1867_declared_and_present_sets_must_match() {
        // Present but undeclared.
        let extra = skill_bundle(
            &[SKILL_ENTRY, "scripts/build.py"],
            &[
                (SKILL_ENTRY, SKILL_MD),
                ("scripts/build.py", BUILD_PY),
                ("references/style.md", STYLE_MD),
            ],
            &skill_workflow("skill: meeting-writer\n"),
        );
        let error = install_error(&extra);
        assert!(error.contains("references/style.md"), "{error}");
        assert!(error.contains("not declared"), "{error}");

        // Declared but absent.
        let missing = skill_bundle(
            &[SKILL_ENTRY, "notes.txt"],
            &[(SKILL_ENTRY, "See notes.txt.\n")],
            &skill_workflow("skill: meeting-writer\n"),
        );
        let error = install_error(&missing);
        assert!(error.contains("notes.txt"), "{error}");
    }

    /// `SKILL.md` is required, `skill.json` is never one of its own members,
    /// and an unknown field is refused by the closed decode.
    #[test]
    fn cad1867_the_manifest_is_closed_and_requires_its_entry() {
        let entry =
            |value: serde_json::Value| parse_skill_manifest(&value.to_string(), "meeting-writer");
        assert!(entry(serde_json::json!({
            "schema": 1,
            "name": "meeting-writer",
            "files": ["scripts/a.py"],
        }))
        .unwrap_err()
        .to_string()
        .contains(SKILL_ENTRY));
        assert!(entry(serde_json::json!({
            "schema": 1,
            "name": "meeting-writer",
            "files": [SKILL_ENTRY, SKILL_MANIFEST],
        }))
        .is_err());
        assert!(entry(serde_json::json!({
            "schema": 1,
            "name": "other",
            "files": [SKILL_ENTRY],
        }))
        .is_err());
        assert!(entry(serde_json::json!({
            "schema": 2,
            "name": "meeting-writer",
            "files": [SKILL_ENTRY],
        }))
        .is_err());
        assert!(entry(serde_json::json!({
            "schema": 1,
            "name": "meeting-writer",
            "files": [SKILL_ENTRY],
            "extra": 1,
        }))
        .is_err());
        // A nested directory is not a member.
        assert!(entry(serde_json::json!({
            "schema": 1,
            "name": "meeting-writer",
            "files": [SKILL_ENTRY, "scripts"],
        }))
        .is_err());
        assert!(entry(serde_json::json!({
            "schema": 1,
            "name": "meeting-writer",
            "files": [SKILL_ENTRY, "scripts/a.rb"],
        }))
        .is_err());
    }

    /// The published caps: 16 declared members, 16 KiB per member, 4 KiB for
    /// the manifest, 32 KiB per package.
    #[test]
    fn cad1867_the_skill_caps_are_enforced() {
        // 17 declared members.
        let mut declared: Vec<String> = vec![SKILL_ENTRY.to_string()];
        for n in 0..SKILL_FILES_MAX {
            declared.push(format!("references/r{n}.md"));
        }
        let names: Vec<&str> = declared.iter().map(String::as_str).collect();
        let mut carried: Vec<(&str, &str)> = vec![(SKILL_ENTRY, "Plain instructions.\n")];
        carried.extend(
            declared
                .iter()
                .skip(1)
                .map(|member| (member.as_str(), "x\n")),
        );
        let files = skill_bundle(&names, &carried, &skill_workflow("skill: meeting-writer\n"));
        assert!(
            install_error(&files).contains("at most"),
            "over the member count"
        );

        // One member over the per-file cap.
        let big = "a".repeat(SKILL_FILE_BYTES as usize + 1);
        let files = skill_bundle(
            &[SKILL_ENTRY, "notes.txt"],
            &[
                (SKILL_ENTRY, "See notes.txt.\n"),
                ("notes.txt", big.as_str()),
            ],
            &skill_workflow("skill: meeting-writer\n"),
        );
        assert!(skill_package(&files, "meeting-writer")
            .unwrap_err()
            .to_string()
            .contains("at most"));

        // Four 8 KiB members plus the manifest are over the package cap.
        let chunk = "b".repeat(8 * 1024);
        let files = skill_bundle(
            &[
                SKILL_ENTRY,
                "notes.txt",
                "references/a.md",
                "references/b.md",
                "references/c.md",
            ],
            &[
                (SKILL_ENTRY, "See notes.txt.\n"),
                ("notes.txt", chunk.as_str()),
                ("references/a.md", chunk.as_str()),
                ("references/b.md", chunk.as_str()),
                ("references/c.md", chunk.as_str()),
            ],
            &skill_workflow("skill: meeting-writer\n"),
        );
        assert!(skill_package(&files, "meeting-writer")
            .unwrap_err()
            .to_string()
            .contains("at most"));
    }

    /// A package may reference only its own declared members: another
    /// package's path refuses, and so does a bare undeclared name.
    #[test]
    fn cad1867_a_reference_to_another_package_refuses() {
        let files = skill_bundle(
            &[SKILL_ENTRY],
            &[(SKILL_ENTRY, "See skills/other-skill/SKILL.md.\n")],
            &skill_workflow("skill: meeting-writer\n"),
        );
        let error = install_error(&files);
        assert!(error.contains("other-skill"), "{error}");

        let bare = skill_bundle(
            &[SKILL_ENTRY],
            &[(SKILL_ENTRY, "See notes.md for the rules.\n")],
            &skill_workflow("skill: meeting-writer\n"),
        );
        let error = install_error(&bare);
        assert!(error.contains("notes.md"), "{error}");
    }

    #[test]
    fn cad1867_static_undeclared_material_cannot_disappear_from_the_scan() {
        let mut files: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::from([
                (
                    "skills/demo/skill.json".into(),
                    r#"{"schema":1,"name":"demo","files":["SKILL.md"]}"#.into(),
                ),
                ("skills/demo/SKILL.md".into(), "Read SKILL.md.\n".into()),
            ]);
        assert!(crate::issue::app::skill_package(&files, "demo").is_ok());
        let long_member = format!("scripts/{}.py", "a".repeat(246));
        for reference in [long_member, ".//scripts/missing.py".to_owned()] {
            files.insert("skills/demo/SKILL.md".into(), format!("Run {reference}.\n"));
            let error = crate::issue::app::skill_package(&files, "demo")
                .expect_err("static undeclared material must refuse")
                .to_string();
            assert!(error.contains("scripts/"), "{error}");
        }
    }

    /// Every `.`-led spelling of the same relative path is the same reference,
    /// so a dotted form cannot hide material the plain form refuses.
    #[test]
    fn cad1867_dotted_spellings_are_not_a_way_around_the_scan() {
        let mut files: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::from([
                (
                    "skills/demo/skill.json".into(),
                    r#"{"schema":1,"name":"demo","files":["SKILL.md"]}"#.into(),
                ),
                ("skills/demo/SKILL.md".into(), "Read SKILL.md.\n".into()),
            ]);
        assert!(crate::issue::app::skill_package(&files, "demo").is_ok());
        for reference in [
            ".//./scripts/missing.py",
            ".//scripts/missing.py",
            "././scripts/missing.py",
            ".//./skills/other/SKILL.md",
            "../scripts/missing.py",
            "../references/spec.md",
        ] {
            files.insert("skills/demo/SKILL.md".into(), format!("Run {reference}.\n"));
            let error = crate::issue::app::skill_package(&files, "demo")
                .expect_err("a dotted spelling must still refuse")
                .to_string();
            assert!(
                error.contains("missing.py")
                    || error.contains("other")
                    || error.contains("through '..'"),
                "{reference}: {error}"
            );
        }
    }

    /// A workflow that names a package the bundle does not carry refuses at
    /// install, and `skill:` must be the step's LAST metadata line.
    #[test]
    fn cad1867_a_declared_tag_must_resolve_and_be_the_last_metadata_line() {
        let files = skill_bundle(
            &[SKILL_ENTRY],
            &[(SKILL_ENTRY, "Plain instructions.\n")],
            &skill_workflow("skill: other-skill\n"),
        );
        let error = install_error(&files);
        assert!(error.contains("other-skill"), "{error}");

        let misordered = skill_workflow("skill: meeting-writer\nsize: S\n");
        let error = crate::issue::workflow::skill_tags(&misordered)
            .expect_err("skill must be last")
            .to_string();
        assert!(error.contains("last metadata line"), "{error}");
    }

    /// A frozen package is a value: updating the installed bundle afterwards
    /// leaves the frozen material — and its digest — exactly as it was.
    #[test]
    fn cad1867_a_frozen_package_does_not_change_when_the_bundle_updates() {
        let mut files = skill_bundle(
            &[SKILL_ENTRY, "scripts/build.py"],
            &[
                (SKILL_ENTRY, "Run ./scripts/build.py.\n"),
                ("scripts/build.py", BUILD_PY),
            ],
            &skill_workflow("skill: meeting-writer\n"),
        );
        let frozen = skill_package(&files, "meeting-writer").unwrap();
        let frozen_json = serde_json::to_value(&frozen).unwrap();
        files.insert(
            format!("{SKILL_DIR}/meeting-writer/scripts/build.py"),
            "print('replaced')\n".to_string(),
        );
        let updated = skill_package(&files, "meeting-writer").unwrap();
        assert_ne!(frozen.digest, updated.digest);
        assert_eq!(serde_json::to_value(&frozen).unwrap(), frozen_json);
        assert_eq!(
            frozen.files[1].text, BUILD_PY,
            "the frozen value kept its own bytes"
        );
    }

    /// CAD-622: an installed copy stays immutable between approvals — an
    /// edit to `<pm>/<project>/apps/<name>` moves the digest, the board's
    /// approval word reads `changed` (the same predicate), and a run's
    /// gate refuses `app_unapproved` instead of running unreviewed bytes.
    #[test]
    fn cad622_an_edit_after_approval_shows_changed_and_stops_runs() {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        crate::issue::write::project_add(&pm, "demo", "DEM", &[], &[], &[], None).unwrap();
        let base = pm.dir.join("demo").join(DIR).join("social-content");
        std::fs::create_dir_all(base.join("workflows")).unwrap();
        std::fs::write(
            base.join(MANIFEST),
            "---\napp: social-content\ntitle: Social content\nversion: '1'\nneeds:\n  connections: []\n---\n\nThe guide.\n",
        )
        .unwrap();
        std::fs::write(
            base.join("workflows").join("post.md"),
            "---\ntitle: \"Post\"\ngoal: \"Write one post\"\ninputs: {}\n---\n\n## Draft\n\nWrite it.\n\n### Acceptance\n- [ ] written\n",
        )
        .unwrap();
        std::fs::write(
            pm.dir.join("demo").join(DIR).join("social-content.yaml"),
            serde_yaml::to_string(&Record {
                schema: 1,
                app: "social-content".into(),
                install_id: "install-1".into(),
                source: Source::Path {
                    path: "tests/fixtures/apps/social-content".into(),
                },
                bindings: Default::default(),
                team: Default::default(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                installed_by: "operator".into(),
                updated_at: None,
                removed: None,
                restore_after: None,
                purge_after: None,
                catalogue: None,
            })
            .unwrap(),
        )
        .unwrap();
        let approved_digest = digest(&pm.dir, "demo", "social-content").unwrap();
        let payload = json!({
            "digest": approved_digest,
            "install_id": "install-1",
            "by": "operator",
            "revoked": false,
        });
        let mut approvals = std::collections::HashMap::new();
        approvals.insert(approval_key("demo", "social-content"), payload.clone());
        let rendered = plan_text(
            &pm.dir,
            &approvals,
            "demo",
            "social-content",
            "post",
            &BTreeMap::new(),
        )
        .expect("the approved install renders");
        assert!(rendered.contains("Write it"));

        // An edit to the installed copy — the bytes the operator approved
        // are no longer the bytes on disk.
        let guide = base.join(MANIFEST);
        let mut text = std::fs::read_to_string(&guide).unwrap();
        text.push_str("\nAn unapproved addition.\n");
        std::fs::write(&guide, text).unwrap();
        let moved = digest(&pm.dir, "demo", "social-content").unwrap();
        assert_ne!(moved, approved_digest, "the digest covers the guide");
        assert!(
            !approval_binds(&payload, &moved, "install-1"),
            "the board reads this install as changed, not approved"
        );
        let refused = plan_text(
            &pm.dir,
            &approvals,
            "demo",
            "social-content",
            "post",
            &BTreeMap::new(),
        )
        .expect_err("an edit after approval stops the run");
        assert!(
            refused.to_string().contains("not approved for its current structure"),
            "{refused}"
        );
        // The operator's re-approval restores it.
        let mut approvals = approvals.clone();
        approvals.insert(
            approval_key("demo", "social-content"),
            json!({"digest": moved, "install_id": "install-1", "revoked": false}),
        );
        plan_text(
            &pm.dir,
            &approvals,
            "demo",
            "social-content",
            "post",
            &BTreeMap::new(),
        )
        .expect("re-approval admits the new bytes");
    }
}
