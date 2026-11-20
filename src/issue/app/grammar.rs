//! App bundle path grammar, member size caps, and skill-package rules.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use super::MANIFEST;
use crate::issue::{model, plan};

/// The ordinary per-file bound; screen and skill package members use their
/// own published bounds instead.
const MAX_FILE_BYTES: u64 = plan::MAX_PLAN_BYTES as u64;

/// A screen asset may use the whole app budget; the aggregate bound still
/// applies to the complete package.
const MAX_SCREEN_ASSET_BYTES: u64 = MAX_APP_BYTES;

/// The per-member safety bound. Screen assets share the app budget; screen
/// declarations, workflows and other files retain their own smaller bounds.
pub(crate) fn file_cap(rel: &str) -> u64 {
    if let Some((tag, leaf)) = rel
        .strip_prefix("screens/")
        .and_then(|rest| rest.split_once('/'))
    {
        if model::valid_tag(tag) {
            if leaf == crate::issue::app_screen_pkg::SCREEN_MANIFEST {
                return crate::issue::app_screen_decl::MAX_MANIFEST_BYTES as u64;
            }
            if crate::issue::app_screen_pkg::leaf_ok(leaf) {
                return MAX_SCREEN_ASSET_BYTES;
            }
        }
    }
    if rel
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
const ROOT_MEMBERS: &[&str] = &[
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
enum DirRule {
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
const DIR_RULES: &[(&str, DirRule)] = &[
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

/// Version of the published grammar document. Bump for a shape or scope change.
#[cfg(test)]
pub(crate) const GRAMMAR_VERSION: u32 = 4;

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
pub(super) const SKILL_ROOT_EXTS: &[&str] =
    &["md", "txt", "json", "yaml", "yml", "csv", "css", "svg"];
/// Extensions the one nested level admits, per subdirectory. `scripts/` holds
/// code an agent may run; `references/` holds prose and notes.
pub(super) const SKILL_SCRIPTS_EXTS: &[&str] = &["py", "sh", "js", "mjs"];
pub(super) const SKILL_REFERENCES_EXTS: &[&str] = &["md", "txt"];
/// Every extension the reference scan treats as naming package material.
pub(super) const SKILL_ALL_EXTS: &[&str] = &[
    "md", "txt", "json", "yaml", "yml", "csv", "css", "svg", "py", "sh", "js", "mjs",
];

/// `scripts/` and `references/` are the only nested directories, each exactly
/// one level deep.
pub(super) fn skill_nested_ext(directory: &str) -> Option<&'static [&'static str]> {
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
/// [`DIR_RULES`] and the caps. The current version must match its checked-in
/// artifact byte for byte; earlier documents remain historical artifacts.
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
            "screenAssets": crate::issue::app_screen_decl::MAX_ASSETS,
            "screenBytes": crate::issue::app_screen_decl::MAX_TOTAL_BYTES,
            "screenManifestBytes": crate::issue::app_screen_decl::MAX_MANIFEST_BYTES,
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

/// Most files a bundle may carry.
pub(super) const MAX_FILES: usize = 128;

/// Largest bundle, all files together — an A1 app is text. The number is
/// stated once in `cadence_core::app_screen_decl::MAX_TOTAL_BYTES` (a screen's
/// assets share this budget); this alias keeps the native name without a
/// second literal that could drift.
pub(crate) const MAX_APP_BYTES: u64 = cadence_core::app_screen_decl::MAX_TOTAL_BYTES;

#[cfg(test)]
mod bundle_grammar_tests {
    use std::collections::HashSet;
    use std::path::Path;

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

    /// The live artifact. Earlier published versions stay untouched so readers
    /// can continue to interpret the versions they already carry.
    fn grammar_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "contracts/bundle-grammar/v{GRAMMAR_VERSION}/grammar.json"
        ))
    }

    /// The published document is generated, never hand-written: this test
    /// keeps the current artifact equal to the predicate consumers apply.
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
        match super::super::validate(root, &HashSet::new(), &[]) {
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
