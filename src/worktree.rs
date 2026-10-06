//! `.cadence/wt/<name>` worktree helpers — `cadence devin --worktree`
//! mints agent checkouts and `cadence issue start` mints issue-bound
//! ones; both share the same layout (`<root>/.cadence/wt/<name>` on
//! `cadence/<name>`, owned by [`layout`]) and the `.cadence/` ignore
//! rule.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::issue::{git, project};
use crate::proto;

pub mod layout;
pub mod lifecycle;

/// Keep `.cadence/` out of a repo's index: append the entry to its
/// `.gitignore` when nothing already covers it.
pub fn ensure_cadence_ignored(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let covered = existing.lines().any(|l| {
        matches!(
            l.trim(),
            ".cadence" | ".cadence/" | "/.cadence" | "/.cadence/"
        )
    });
    if !covered {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, ".cadence/")?;
    }
    Ok(())
}

/// The main checkout root for `dir` — `--git-common-dir` maps linked
/// worktrees back to the same root, so `…/.cadence/wt/` always lands
/// under the primary checkout rather than nested inside a worktree.
pub fn main_root(dir: &Path) -> Result<PathBuf> {
    let (root, _) = project::repo_identity(dir).ok_or_else(|| {
        Error::rejected(format!(
            "'{}' is not inside a git repository",
            dir.display()
        ))
    })?;
    Ok(root)
}

/// Confirm that a development path is a canonical, registered worktree of
/// `root` and has the expected branch before an idempotent reuse.
pub(crate) fn validate_registered_branch(root: &Path, lane: &Path, branch: &str) -> Result<()> {
    let meta = std::fs::symlink_metadata(lane)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::rejected(format!(
            "Worktree path {} is not a real directory — refusing reuse",
            lane.display()
        )));
    }
    let actual_path = lane.canonicalize()?;
    if actual_path.as_path() != lane {
        return Err(Error::rejected(format!(
            "Worktree path {} differs from its canonical identity — refusing reuse",
            lane.display()
        )));
    }
    let actual_root = main_root(&actual_path)?.canonicalize()?;
    if actual_root != root.canonicalize()? {
        return Err(Error::rejected(format!(
            "Worktree {} belongs to repo {}, not {} — refusing reuse",
            lane.display(),
            actual_root.display(),
            root.display()
        )));
    }
    let registered = git(root, &["worktree", "list", "--porcelain"])?;
    if !registered
        .lines()
        .any(|line| line == format!("worktree {}", actual_path.display()))
    {
        return Err(Error::rejected(format!(
            "Worktree {} is not registered by repo {} — refusing reuse",
            lane.display(),
            root.display()
        )));
    }
    if crate::issue::finish::git_branch(&actual_path)?.as_deref() != Some(branch) {
        return Err(Error::rejected(format!(
            "Worktree {} is not checked out on expected branch {} — refusing reuse",
            lane.display(),
            branch
        )));
    }
    Ok(())
}

/// `git worktree add <dir> [-b <branch>] <base>` — the bare plumbing;
/// existence and idempotency decisions are the caller's.
pub fn add(root: &Path, dir: &Path, branch: Option<&str>, base: &str) -> Result<()> {
    let target = dir.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "add", &target];
    if let Some(branch) = branch {
        args.push("-b");
        args.push(branch);
    }
    args.push(base);
    git(root, &args)?;
    Ok(())
}

/// `git worktree add <root>/.cadence/wt/<name> -b cadence/<name>` — the
/// new checkout becomes the agent's cwd. Clean failures: no git repo
/// under `base`, a pre-existing worktree dir, or a branch collision.
pub fn create_worktree(base: &Path, name: &str) -> Result<PathBuf> {
    proto::identifier(name, "Worktree name")?;
    let root = main_root(base)?;
    let dir = layout::worktree_dir(&root, name);
    let branch = layout::branch(name);
    let recovery = lifecycle::recoverable_record(
        &root,
        &dir,
        "development",
        "cadence agent worktree",
        Some(&branch),
        None,
    )?;
    let branch_exists = git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    let actor = std::env::var("CADENCE_ALIAS").unwrap_or_else(|_| "operator".to_string());
    let (pinned_sha, owner) = if let Some(record) = &recovery {
        if record.owner != actor {
            return Err(Error::rejected(format!(
                "managed worktree {} is owned by '{}', not '{}'; inventory it before recovery",
                dir.display(),
                record.owner,
                actor
            )));
        }
        (record.pinned_sha.clone(), record.owner.clone())
    } else {
        if dir.exists() {
            return Err(Error::rejected(format!(
                "Worktree '{name}' already exists at {} — inventory it and explicitly adopt it before reuse",
                dir.display()
            )));
        }
        if branch_exists {
            return Err(Error::rejected(format!(
                "Branch '{branch}' already exists without matching lifecycle ownership — refusing reuse"
            )));
        }
        (crate::issue::start::resolve_base(&root, None)?.1, actor)
    };
    ensure_cadence_ignored(&root)?;
    // The recovery branches below add a checkout without `begin`; refuse a
    // symlinked destination parent before Git creates anything.
    lifecycle::validate_creation_parent(&root, &dir)?;
    if let Some(record) = recovery {
        if dir.exists() {
            validate_registered_branch(&root, &dir, &branch)?;
        } else if branch_exists {
            if let Err(e) = add(&root, &dir, None, &branch) {
                let _ = lifecycle::transition(&root, &dir, "setup-failed", Some(&e.to_string()));
                return Err(e);
            }
        } else if matches!(record.state.as_str(), "preparing" | "setup-failed") {
            let pinned = git(
                &root,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", record.pinned_sha),
                ],
            )?;
            if pinned != record.pinned_sha {
                return Err(Error::rejected(format!(
                    "recorded base {} no longer resolves to the same commit",
                    record.pinned_sha
                )));
            }
            if let Err(e) = add(&root, &dir, Some(&branch), &pinned) {
                let _ = lifecycle::transition(&root, &dir, "setup-failed", Some(&e.to_string()));
                return Err(e);
            }
        } else {
            return Err(Error::rejected(format!(
                "managed worktree {} and branch {branch} are both missing; refusing to recreate active work",
                dir.display()
            )));
        }
        if let Err(e) = setup_development(&dir, &root, true)
            .and_then(|_| lifecycle::activate(&root, record.clone()))
        {
            let _ = lifecycle::transition(&root, &dir, "setup-failed", Some(&e.to_string()));
            return Err(e);
        }
        return Ok(dir);
    }

    let record = lifecycle::new_record(lifecycle::CheckoutSpec {
        repo: &root,
        purpose: "development",
        tool: "cadence agent worktree",
        owner: &owner,
        path: &dir,
        branch: Some(&branch),
        pinned_sha: &pinned_sha,
        issue: None,
    });
    lifecycle::begin(&root, record.clone())?;
    if let Err(e) = add(&root, &dir, Some(&branch), &pinned_sha) {
        let _ = lifecycle::transition(&root, &dir, "setup-failed", Some(&e.to_string()));
        return Err(Error::rejected(format!(
            "{e} — inspect lifecycle inventory before retrying the named checkout"
        )));
    }
    if let Err(e) =
        setup_development(&dir, &root, true).and_then(|_| lifecycle::activate(&root, record))
    {
        let _ = lifecycle::transition(&root, &dir, "setup-failed", Some(&e.to_string()));
        return Err(e);
    }
    Ok(dir)
}

/// The shared cargo cache for a repo — `<root>/.cadence/target/
/// shared`. One dependency-artifact store for every `.cadence/wt`
/// lane: dependency artifacts are built once per host instead of once
/// per worktree.
pub fn shared_target_dir(root: &Path) -> PathBuf {
    root.join(".cadence").join("target").join("shared")
}

/// CAD-1021 slice 1: the lane's pre-push gate. Linked worktrees share
/// the common `.git/hooks`, so the hook can never live there — it is a
/// per-worktree `core.hooksPath` (written to the worktree's own
/// `config.worktree`, never the shared `config`) pointing at a hooks
/// dir under the worktree's own git dir. `extensions.worktreeConfig`
/// is the one shared-config key the install sets (idempotent); the main
/// checkout keeps no hook either way.
pub const HOOKS_DIR: &str = "cadence-hooks";

/// The hook script installed at `<git-dir>/cadence-hooks/pre-push`.
/// Git runs pre-push hooks with GIT_DIR already pointed at the
/// worktree's git dir and cwd at the worktree root, so the receipt and
/// `scripts/pre-push` both resolve per-lane. The receipt is written
/// atomically (tmp+rename) under the worktree's git dir — a `--no-verify`
/// push leaves no receipt, which is the detection signal (contract D3).
const PRE_PUSH_HOOK: &str = r#"#!/bin/sh
# CAD-1021: run scripts/pre-push on every push; a non-zero step blocks it.
# The receipt records the checked head, lane and per-step exit codes under
# this worktree's git dir — a pushed head with no receipt was pushed with
# --no-verify, and that absence is the audit signal.
top=$(git rev-parse --show-toplevel) || exit 1
gd=$(git rev-parse --absolute-git-dir) || exit 1
cd "$top" || exit 1
# git exports GIT_DIR/GIT_WORK_TREE/etc for the hook — unset them before
# spawning scripts/pre-push so its own `git` calls (and any subprocess that
# shells out to git, e.g. the contract tests) resolve the worktree normally
# instead of running inside the bare-dir context the hook inherits.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX GIT_QUARANTINE_PATH
tmp="$gd/cadence-pre-push-receipt.tmp"
out="$gd/cadence-pre-push-receipt"
# A worktree without scripts/pre-push (a fixture, a sparse lane, a repo that
# never carried the script) has no gate to run — record that and let the push
# through rather than blocking on a script that isn't there.
if [ ! -x scripts/pre-push ]; then
    printf 'head=%s lane=%s kind=none steps=skipped:no-pre-push rc=0 at=%s\n' \
        "$(git rev-parse HEAD 2>/dev/null || echo '?')" \
        "${CADENCE_ALIAS:-$USER}" "$(date +%s)" > "$tmp" 2>/dev/null && mv "$tmp" "$out"
    exit 0
fi
line=$(scripts/pre-push --receipt ${PRE_PUSH_TESTS:+--tests})
rc=$?
printf '%s\n' "$line" > "$tmp" && mv "$tmp" "$out"
exit $rc
"#;

/// Install the lane's pre-push hook: write `<git-dir>/cadence-hooks/
/// pre-push` (0755) under the worktree's OWN git dir, enable
/// `extensions.worktreeConfig` on the shared config once, then point
/// this worktree's `config.worktree` `core.hooksPath` at that dir.
/// Idempotent — a re-start re-heals; an identical hook is left alone.
/// Fail-closed: `issue start` treats a failed install as a failed lane
/// (same rollback as `configure_cargo_target`), because a lane that
/// cannot enforce the gate must not be minted.
pub fn install_pre_push_hook(wt_dir: &Path) -> Result<PathBuf> {
    let git_dir = PathBuf::from(git(wt_dir, &["rev-parse", "--absolute-git-dir"])?);
    let hooks = git_dir.join(HOOKS_DIR);
    std::fs::create_dir_all(&hooks)?;
    let script = hooks.join("pre-push");
    // Write the script first (atomic tmp+rename), then config — a crash
    // between the two leaves an unused flag or an un-pointed script, both
    // re-healed by the next `issue start`.
    let needs_write = match std::fs::read_to_string(&script) {
        Ok(existing) => existing != PRE_PUSH_HOOK,
        Err(_) => true,
    };
    if needs_write {
        let tmp = hooks.join(".pre-push.tmp");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o755)
                .open(&tmp)?;
            f.write_all(PRE_PUSH_HOOK.as_bytes())?;
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &script)?;
    }
    // Shared config: enable per-worktree config (idempotent — already
    // true is a no-op), then this worktree's own hooksPath.
    git(wt_dir, &["config", "extensions.worktreeConfig", "true"])?;
    git(
        wt_dir,
        &[
            "config",
            "--worktree",
            "core.hooksPath",
            &hooks.to_string_lossy(),
        ],
    )?;
    install_cargo_shim(wt_dir)?;
    Ok(script)
}

/// Apply shared cargo-target and pre-push setup to a development checkout.
/// Ownership, tracker binding and rollback remain with the caller.
pub fn setup_development(wt_dir: &Path, root: &Path, shared: bool) -> Result<Option<PathBuf>> {
    ensure_cadence_ignored(root)?;
    let target = configure_cargo_target(wt_dir, root, shared)?;
    install_pre_push_hook(wt_dir)?;
    Ok(target)
}

/// CAD-1021 slice 3: the lane's `cargo` shim, at
/// `<git-dir>/cadence-hooks/bin/cargo`. Placed on the lane's PATH (via
/// the worktree `.env`'s `PATH=` line) it routes cargo work through
/// `cadence build-slot run <kind>` so a lane's compiles are always
/// admitted. The routing is an ALLOWLIST of clearly-light subcommands —
/// everything else, including unknown subcommands and aliases, queues on
/// the build pool. That is the operator's goal: no local compile escapes
/// the queue (AGENTS.md — allowlists, not denylists). `cargo test` takes
/// the test pool and `cargo nextest` the suite pool.
///
/// Lane-local: the script and its PATH entry live under the lane's own
/// git dir / `.env` — the main checkout and CI never see it. The shim
/// fails OPEN to a real cargo when `cadence` is unreachable so a lane
/// is never bricked by a missing daemon; the receipt/pre-push gate,
/// not the shim, is the enforcement.
const CARGO_SHIM: &str = r##"#!/bin/sh
# CAD-1021: route cargo work through `cadence build-slot run`.
# Only the clearly-light, never-compiling subcommands pass straight to
# the real cargo — an allowlist (AGENTS.md: allowlists, not denylists),
# so `cargo run`/`bench`/`doc`/`rustc`/`llvm-cov`/`package`/`publish`,
# every alias and every unknown subcommand all queue as `build`.
# Inside a slot (CADENCE_BUILD_SLOT_PID names a live holder) a nested
# cargo passes through too, so `build-slot run` of a recipe that itself
# calls cargo never re-queues on itself.

real_cargo() {
    # Resolve the real cargo: the shim dir is never on this lookup's
    # PATH (we strip it) so it can never re-enter itself.
    PATH="$(printf '%s' "$PATH" | tr ':' '\n' | grep -vx "$shim_dir" | tr '\n' ':')"
    export PATH
    command -v cargo
}

shim_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)

# Pass-through: a held slot's children re-use it — the pid must name a
# LIVE holder, so a forged CADENCE_BUILD_SLOT_PID names no live pid and
# falls through to the queue path like any caller.
if [ -n "$CADENCE_BUILD_SLOT_PID" ] && kill -0 "$CADENCE_BUILD_SLOT_PID" 2>/dev/null; then
    exec "$(real_cargo)" "$@"
fi

case "${1:-}" in
    # Allowlist — subcommands that never compile. A bare `cargo` (no
    # subcommand) prints help; version/help flags are light too.
    ''|fmt|metadata|tree|locate-project|version|-V|--version|help|-h|--help|\
    add|remove|search|pkgid|verify-project|read-manifest|generate-lockfile|\
    owner|login|logout|yank)
        exec "$(real_cargo)" "$@" ;;
    test)
        slot_kind=test ;;
    nextest)
        slot_kind=suite ;;
    *)
        slot_kind=build ;;
esac

# `cadence` the lane recorded in its env; fall back to PATH.
cad="${CADENCE_BUILD_SLOT:-cadence}"
"$cad" build-slot run "$slot_kind" -- cargo "$@"
rc=$?
[ "$rc" -eq 0 ] || exit "$rc"
"##;

/// Write the shim under `<git-dir>/cadence-hooks/bin/cargo` (0755) and
/// record its PATH prefix in the worktree's `.env`. Idempotent; a lane
/// minted before slice 3 re-heals on the next `issue start` (the reuse
/// path calls `install_pre_push_hook`, which now calls this).
fn install_cargo_shim(wt_dir: &Path) -> Result<PathBuf> {
    let git_dir = PathBuf::from(git(wt_dir, &["rev-parse", "--absolute-git-dir"])?);
    let bin = git_dir.join(HOOKS_DIR).join("bin");
    std::fs::create_dir_all(&bin)?;
    let shim = bin.join("cargo");
    let needs_write = match std::fs::read_to_string(&shim) {
        Ok(existing) => existing != CARGO_SHIM,
        Err(_) => true,
    };
    if needs_write {
        let tmp = bin.join(".cargo.tmp");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o755)
                .open(&tmp)?;
            f.write_all(CARGO_SHIM.as_bytes())?;
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &shim)?;
    }
    // Record the shim dir on the lane's `.env` PATH so a shell that
    // sources the lane env resolves `cargo` here first. `.env` is the
    // existing lane-env file `write_slot_env` owns; we prepend our dir
    // under the same tmp+rename discipline, keeping every other line AND
    // an existing file's mode (a `0600`→`0640` the operator set survives).
    let file = wt_dir.join(".env");
    let mut mode = 0o600;
    let mut text = String::new();
    let mut wrote_path = false;
    if let Ok(meta) = std::fs::symlink_metadata(&file) {
        if meta.file_type().is_symlink() {
            return Err(Error::rejected(format!(
                "{} is a symlink — refusing to write the slot env through it",
                file.display()
            )));
        }
        mode = meta.permissions().mode() & 0o777;
    }
    if let Ok(existing) = std::fs::read_to_string(&file) {
        for line in existing.lines() {
            if let Some(rest) = line.strip_prefix("PATH=") {
                if !rest.split(':').any(|p| p == bin.to_string_lossy()) {
                    text.push_str(&format!("PATH={}:{rest}\n", bin.display()));
                } else {
                    text.push_str(line);
                    text.push('\n');
                }
                wrote_path = true;
            } else {
                text.push_str(line);
                text.push('\n');
            }
        }
    }
    if !wrote_path {
        text.push_str(&format!("PATH={}:$PATH\n", bin.display()));
    }
    let tmp = wt_dir.join(".env.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    std::fs::rename(&tmp, &file)?;
    Ok(shim)
}

/// The `debug/` children cargo fills with *hashed* names —
/// `<name>-<metadata>.<ext>` keyed by package id (which includes the
/// source path), features and profile — so two lanes' artifacts never
/// share a filename. These are the dirs a worktree symlinks into the
/// shared cache; cargo's build-lock files join them so concurrent
/// lanes queue on cargo's own locking. Everything else under `debug/` —
/// uplifted binaries, uplifted rlibs, `.d` files — is unhashed and
/// stays per-lane, which is what keeps one lane's `cargo test` from
/// exec'ing another lane's `debug/cadence`.
pub(crate) const SHARED_DEBUG_DIRS: [&str; 4] = ["deps", ".fingerprint", "build", "incremental"];
/// `examples/` is deliberately absent: cargo uplifts example binaries
/// to `debug/examples/<name>` *unhashed*, so sharing it hands one lane
/// another lane's example — the bug this farm exists to prevent. Any
/// link an older cadence planted is unlinked on sight.
pub(crate) const RETIRED_DEBUG_DIRS: [&str; 1] = ["examples"];
/// Cargo's build locks (all three exist on modern toolchains) are
/// shared too, so two lanes building at once serialise on cargo's own
/// locking rather than racing writes into the shared `deps/`.
pub(crate) const SHARED_DEBUG_FILES: [&str; 3] =
    [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// Should the project's worktrees share the dep cache?
/// `build: {target_dir: per-worktree}` in `project.yaml` opts a lane
/// back onto fully-private build output; anything else is rejected.
pub fn shared_deps_enabled(project: &project::Project) -> Result<bool> {
    match project.build.as_ref().and_then(|b| b.target_dir.as_deref()) {
        None | Some("shared") => Ok(true),
        Some("per-worktree") => Ok(false),
        Some(other) => Err(Error::rejected(format!(
            "[build] target_dir = \"{other}\" in {}'s project.yaml — \
             expected \"shared\" or \"per-worktree\"",
            project.key
        ))),
    }
}

/// `build.target-dir` from one cargo config file, resolved the way
/// cargo resolves it: relative values are relative to the directory
/// that contains `.cargo/`. `None` on a missing, unreadable or
/// target-dir-less file — a malformed config is the operator's
/// problem, not a reason to fail `issue start`.
fn config_target_dir(conf: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(conf).ok()?;
    let doc = toml::from_str::<toml::Table>(&text).ok()?;
    let dir = doc.get("build")?.get("target-dir")?.as_str()?;
    let dir = PathBuf::from(dir);
    Some(if dir.is_absolute() {
        dir
    } else {
        conf.parent()?.parent()?.join(dir)
    })
}

/// Where cargo will actually put this worktree's build output, per
/// cargo's own precedence: `CARGO_TARGET_DIR` first (it outranks
/// every config file; relative values are anchored to the worktree,
/// the only sane cwd-independent reading), then `.cargo/config.toml`
/// or legacy `.cargo/config` in the worktree and every ancestor up to
/// `/`, then `$CARGO_HOME/config.toml` (default `~/.cargo/config.toml`)
/// — first `build.target-dir` wins. An ancestor or home-level
/// redirect applies to every lane exactly like a worktree-local one
/// does, so it is honoured here too rather than planting a farm cargo
/// would ignore.
pub fn effective_target_dir(wt_dir: &Path) -> PathBuf {
    if let Some(v) = std::env::var_os("CARGO_TARGET_DIR").filter(|v| !v.is_empty()) {
        let dir = PathBuf::from(v);
        return if dir.is_absolute() {
            dir
        } else {
            wt_dir.join(dir)
        };
    }
    for dir in wt_dir.ancestors() {
        for name in ["config.toml", "config"] {
            if let Some(target) = config_target_dir(&dir.join(".cargo").join(name)) {
                return target;
            }
        }
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
    if let Some(home) = cargo_home {
        if let Some(target) = config_target_dir(&home.join("config.toml")) {
            return target;
        }
    }
    wt_dir.join("target")
}

/// Copy `src` — file, dir or symlink — to `dst`, recursively.
fn copy_into(src: &Path, dst: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_into(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}

/// Remove `path` whatever its kind — file, dir or symlink.
fn remove_any(path: &Path) -> Result<()> {
    if path.symlink_metadata()?.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Copy every entry of `src` into `dst`, skipping names already
/// present — the reverse of `merge_dir_into`. Used when a shared link
/// is retired: the lane gets back whatever the cache holds without
/// emptying it (another lane may still link there).
fn copy_missing(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest = dst.join(entry.file_name());
        if !dest.exists() && !dest.is_symlink() {
            copy_into(&entry.path(), &dest)?;
        }
    }
    Ok(())
}

/// Move every entry of `src` into `dst`, skipping names the cache
/// already has. A `.cadence` dir on another filesystem (EXDEV) falls
/// back to copy+remove. When `src` is empty it is removed; when a
/// collision left entries behind, `src` is renamed aside to
/// `<name>.local` — the lane's copy of a colliding artifact is never
/// silently deleted.
fn merge_dir_into(src: &Path, dst: &Path) -> Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest = dst.join(entry.file_name());
        if dest.exists() || dest.is_symlink() {
            continue;
        }
        match std::fs::rename(entry.path(), &dest) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                copy_into(&entry.path(), &dest)?;
                remove_any(&entry.path())?;
            }
            Err(e) => return Err(e.into()),
        }
    }
    if src.read_dir()?.next().is_none() {
        std::fs::remove_dir_all(src)?;
    } else {
        let kept = src.with_file_name(format!(
            "{}.local",
            src.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::rename(src, &kept)?;
    }
    Ok(())
}

/// Is `link` a symlink pointing at `target`?
fn is_link_to(link: &Path, target: &Path) -> bool {
    link.symlink_metadata()
        .ok()
        .filter(|m| m.file_type().is_symlink())
        .is_some_and(|_| std::fs::read_link(link).is_ok_and(|t| t == target))
}

/// Is `path` under an exclusive flock right now? A non-blocking
/// LOCK_EX attempt — only EWOULDBLOCK means held (any other errno is
/// just an unreadable file, not a build). Success means free, and the
/// `File` drop releases the probe lock immediately. `Err` when the
/// file can't even be opened (e.g. fd exhaustion) — callers that
/// need certainty must not treat that as "unlocked".
fn lock_held(path: &Path) -> Result<bool> {
    let f = std::fs::File::open(path).map_err(|e| {
        Error::rejected(format!(
            "cannot probe {} ({e}) — retry `issue start` when the lane is idle",
            path.display()
        ))
    })?;
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    Ok(rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK))
}

/// Advisory lock check for diagnostics — a probe failure reads as
/// "not locked" since the flag is informational only.
pub(crate) fn file_locked(path: &Path) -> bool {
    lock_held(path).unwrap_or(false)
}

/// Test-only holder for a synthetic cargo lock. Releasing with `flock` is
/// deliberate: closing one descriptor does not release an open-file-
/// description lock while duplicate references still exist.
#[cfg(test)]
pub(crate) struct TestFileLock {
    file: Option<std::fs::File>,
}

#[cfg(test)]
impl TestFileLock {
    pub(crate) fn acquire(path: &Path) -> Self {
        let file = std::fs::File::open(path).expect("lock fixture");
        use std::os::unix::io::AsRawFd;
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) },
            0,
            "lock fixture"
        );
        Self { file: Some(file) }
    }

    pub(crate) fn release(mut self) {
        let file = self.file.take().expect("lock fixture already released");
        use std::os::unix::io::AsRawFd;
        assert_eq!(
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) },
            0,
            "unlock fixture"
        );
    }
}

#[cfg(test)]
impl Drop for TestFileLock {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else {
            return;
        };
        use std::os::unix::io::AsRawFd;
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Point a worktree's cargo builds at the shared dep cache: inside
/// `<wt>/target/debug`, the hashed-content subdirs become symlinks to
/// `<root>/.cadence/target/shared/debug/<name>` (merging any existing
/// real dir first), while the lane's own `debug/` stays a real dir —
/// uplifted binaries like `debug/cadence` are per-lane files, so one
/// lane's `cargo test` can never exec another lane's binary. With
/// `shared=false` a previously-planted farm is undone (links into our
/// shared dir become empty real dirs) so a `per-worktree` lane is
/// fully private again. Nothing under `.cargo/` is written or
/// excluded — a tracked or hand-written `.cargo/config.toml` is never
/// touched.
///
/// Returns `Some(effective target dir)` for the worktree ref, or
/// `None` when the checkout is not a cargo package — a repo with no
/// `Cargo.toml` gets no `target/` created and nothing moved, so a
/// Maven-style `target/debug/build` it already owns is left
/// byte-identical. A failed plant removes the links it created so
/// the lane is never left half-shared (artifacts already folded into
/// the cache stay there; a retry re-links over them).
pub fn configure_cargo_target(wt_dir: &Path, root: &Path, shared: bool) -> Result<Option<PathBuf>> {
    let wt_debug = wt_dir.join("target").join("debug");
    let shared_debug = shared_target_dir(root).join("debug");
    if !shared {
        for name in SHARED_DEBUG_DIRS.iter().chain(&RETIRED_DEBUG_DIRS) {
            let link = wt_debug.join(name);
            if is_link_to(&link, &shared_debug.join(name)) {
                std::fs::remove_file(&link)?;
                std::fs::create_dir_all(&link)?;
                // A retired dir's shared content comes back to the
                // lane — the shared copy stays for any other lane
                // still linking it.
                if RETIRED_DEBUG_DIRS.contains(name) {
                    copy_missing(&shared_debug.join(name), &link)?;
                }
            }
        }
        for name in SHARED_DEBUG_FILES {
            let link = wt_debug.join(name);
            if is_link_to(&link, &shared_debug.join(name)) {
                std::fs::remove_file(&link)?;
            }
        }
        return Ok(if wt_dir.join("Cargo.toml").is_file() {
            Some(effective_target_dir(wt_dir))
        } else {
            None
        });
    }
    if !wt_dir.join("Cargo.toml").is_file() {
        return Ok(None);
    }
    let effective = effective_target_dir(wt_dir);
    // An operator's `build.target-dir` — worktree-local, an ancestor's
    // or the cargo-home one — redirects cargo elsewhere; the farm
    // would sit inert, so it is not planted.
    if effective != wt_dir.join("target") {
        return Ok(Some(effective));
    }
    // Probe the lane's real lock files BEFORE anything moves: merging
    // `deps/` etc. — or even retiring an old link — while a build
    // holds the lock pulls files out from under rustc, and a refusal
    // afterwards cannot put them back. All-or-nothing for data, not
    // just for links.
    for name in SHARED_DEBUG_FILES {
        let link = wt_debug.join(name);
        // Links (ours or foreign) are not real lock files — a live
        // build always holds a real one.
        if link.is_symlink() || !link.exists() {
            continue;
        }
        if lock_held(&link)? {
            return Err(Error::rejected(format!(
                "{} is locked by a running cargo build — \
                 retry `issue start` when the lane is idle",
                link.display()
            )));
        }
    }
    // An `examples` link planted under the r2 design shares unhashed
    // example binaries — retire it on sight, before it misleads a
    // build. The lane gets a real dir seeded with whatever the shared
    // copy already holds (copied, not moved — other r2-era lanes may
    // still link it).
    let retired = wt_debug.join(RETIRED_DEBUG_DIRS[0]);
    if is_link_to(&retired, &shared_debug.join(RETIRED_DEBUG_DIRS[0])) {
        std::fs::remove_file(&retired)?;
        std::fs::create_dir_all(&retired)?;
        copy_missing(&shared_debug.join(RETIRED_DEBUG_DIRS[0]), &retired)?;
    }
    let mut created: Vec<PathBuf> = Vec::new();
    let planted = (|| -> Result<()> {
        for name in SHARED_DEBUG_DIRS {
            let link = wt_debug.join(name);
            let shared_sub = shared_debug.join(name);
            std::fs::create_dir_all(&shared_sub)?;
            std::fs::create_dir_all(&wt_debug)?;
            if is_link_to(&link, &shared_sub) {
                continue;
            }
            if link.is_symlink() {
                // A symlink to somewhere else is the operator's —
                // leaving it while linking the rest would half-share
                // the lane, so refuse loudly instead.
                return Err(Error::rejected(format!(
                    "{} is a symlink outside the shared cache — remove it \
                     or set build.target_dir = \"per-worktree\" in project.yaml",
                    link.display()
                )));
            }
            if link.is_dir() {
                // A real dir from a pre-shared build — fold its
                // artifacts into the cache, then link.
                merge_dir_into(&link, &shared_sub)?;
            }
            std::os::unix::fs::symlink(&shared_sub, &link)?;
            created.push(link);
        }
        for name in SHARED_DEBUG_FILES {
            let link = wt_debug.join(name);
            let shared_file = shared_debug.join(name);
            std::fs::create_dir_all(&wt_debug)?;
            if is_link_to(&link, &shared_file) {
                continue;
            }
            if link.is_symlink() {
                return Err(Error::rejected(format!(
                    "{} is a symlink outside the shared cache — remove it \
                     or set build.target_dir = \"per-worktree\" in project.yaml",
                    link.display()
                )));
            }
            if link.exists() {
                // Re-probe: the pre-merge check ran first, but a build
                // could have started during it — a held lock still
                // refuses the whole plant.
                if lock_held(&link)? {
                    return Err(Error::rejected(format!(
                        "{} is locked by a running cargo build — \
                         retry `issue start` when the lane is idle",
                        link.display()
                    )));
                }
                std::fs::remove_file(&link)?;
            }
            std::os::unix::fs::symlink(&shared_file, &link)?;
            created.push(link);
        }
        Ok(())
    })();
    if planted.is_err() {
        for link in created {
            if link.is_symlink() {
                let _ = std::fs::remove_file(&link);
            }
        }
    }
    planted?;
    Ok(Some(effective))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn project_with(build: Option<project::Build>) -> project::Project {
        project::Project {
            key: "demo".to_string(),
            prefix: "D".to_string(),
            repos: vec![],
            components: vec![],
            tags: vec![],
            default_owner: None,
            build,
            memory: None,
            intake: None,
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn git_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init", "-q", "-b", "main"]);
        git(dir.path(), &["config", "user.email", "t@t"]);
        git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("f"), "x").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "/target\n").unwrap();
        // A cargo package — the farm only plants in cargo checkouts.
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-qm", "init"]);
        dir
    }

    #[test]
    fn shared_deps_default_and_per_worktree() {
        for build in [
            None,
            Some(project::Build::default()),
            Some(project::Build {
                target_dir: Some("shared".to_string()),
                ..Default::default()
            }),
        ] {
            assert!(shared_deps_enabled(&project_with(build)).unwrap());
        }
        let per = project_with(Some(project::Build {
            target_dir: Some("per-worktree".to_string()),
            ..Default::default()
        }));
        assert!(!shared_deps_enabled(&per).unwrap());
        let bad = project_with(Some(project::Build {
            target_dir: Some("/somewhere/else".to_string()),
            ..Default::default()
        }));
        let e = shared_deps_enabled(&bad).unwrap_err();
        assert!(e.to_string().contains("/somewhere/else"), "{e}");
    }

    #[test]
    fn configure_plants_farm_and_stays_clean() {
        let repo = git_repo();
        let wt = repo.path();
        let effective = configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(effective, Some(wt.join("target")));
        let debug = wt.join("target/debug");
        for name in SHARED_DEBUG_DIRS {
            let link = debug.join(name);
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                shared_target_dir(repo.path()).join("debug").join(name),
                "{name}"
            );
        }
        for name in SHARED_DEBUG_FILES {
            let link = debug.join(name);
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                shared_target_dir(repo.path()).join("debug").join(name),
                "{name}"
            );
        }
        // `debug/` itself is a real dir — uplifted binaries are per-lane.
        assert!(!debug.is_symlink());
        // No `.cargo/` anywhere — a tracked config would be untouched.
        assert!(!wt.join(".cargo").exists());
        assert_eq!(git(wt, &["status", "--porcelain"]), "");
        // Idempotent.
        configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(git(wt, &["status", "--porcelain"]), "");
    }

    #[test]
    fn configure_merges_existing_target_into_shared() {
        let repo = git_repo();
        let wt = repo.path();
        // A pre-shared lane already built: real dirs with artifacts.
        let debug = wt.join("target/debug");
        std::fs::create_dir_all(debug.join("deps")).unwrap();
        std::fs::write(debug.join("deps/libdep-abc.rlib"), "rlib").unwrap();
        std::fs::create_dir_all(debug.join(".fingerprint/dep-abc")).unwrap();
        std::fs::write(debug.join("probe"), "bin").unwrap();
        configure_cargo_target(wt, repo.path(), true).unwrap();
        let shared = shared_target_dir(repo.path()).join("debug");
        // Artifacts moved into the cache; the lane's own bin stayed.
        assert_eq!(
            std::fs::read_to_string(shared.join("deps/libdep-abc.rlib")).unwrap(),
            "rlib"
        );
        assert!(shared.join(".fingerprint/dep-abc").is_dir());
        assert_eq!(std::fs::read_to_string(debug.join("probe")).unwrap(), "bin");
        assert!(debug.join("deps").is_symlink());
    }

    #[test]
    fn configure_per_worktree_unplants_farm() {
        let repo = git_repo();
        let wt = repo.path();
        configure_cargo_target(wt, repo.path(), true).unwrap();
        let effective = configure_cargo_target(wt, repo.path(), false).unwrap();
        assert_eq!(effective, Some(wt.join("target")));
        for name in SHARED_DEBUG_DIRS {
            let p = wt.join("target/debug").join(name);
            assert!(p.is_dir() && !p.is_symlink(), "{name}");
        }
        for name in SHARED_DEBUG_FILES {
            assert!(!wt.join("target/debug").join(name).exists(), "{name}");
        }
        // The shared cache kept its dirs — nothing was deleted.
        assert!(shared_target_dir(repo.path()).join("debug/deps").is_dir());
    }

    #[test]
    fn configure_preserves_operator_target_dir_and_config() {
        let repo = git_repo();
        let wt = repo.path();
        let cargo = wt.join(".cargo");
        std::fs::create_dir_all(&cargo).unwrap();
        let conf_text = "[build]\ntarget-dir = \"/var/cache/mine\"\njobs = 2\n";
        std::fs::write(cargo.join("config.toml"), conf_text).unwrap();
        git(wt, &["add", "-A"]);
        git(wt, &["commit", "-qm", "cargo config"]);
        let effective = configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(effective, Some(PathBuf::from("/var/cache/mine")));
        // Tracked config survives byte-for-byte; no farm planted.
        assert_eq!(
            std::fs::read_to_string(cargo.join("config.toml")).unwrap(),
            conf_text
        );
        assert!(!wt.join("target").exists());
        assert_eq!(git(wt, &["status", "--porcelain"]), "");
        // A relative operator choice resolves against the worktree.
        std::fs::write(
            cargo.join("config.toml"),
            "[build]\ntarget-dir = \"build-out\"\n",
        )
        .unwrap();
        let effective = configure_cargo_target(wt, repo.path(), true).unwrap();
        assert_eq!(effective, Some(wt.join("build-out")));
        assert!(!wt.join("target").exists());
    }

    #[test]
    fn configure_refuses_foreign_symlinks() {
        let repo = git_repo();
        let wt = repo.path();
        let foreign = wt.join("elsewhere");
        std::fs::create_dir_all(&foreign).unwrap();
        let debug = wt.join("target/debug");
        std::fs::create_dir_all(&debug).unwrap();
        std::os::unix::fs::symlink(&foreign, debug.join("deps")).unwrap();
        // A foreign `deps` link would leave the lane half-shared —
        // refused loudly, and the link itself is untouched.
        let e = configure_cargo_target(wt, repo.path(), true).unwrap_err();
        assert!(e.to_string().contains("outside the shared cache"), "{e}");
        assert_eq!(std::fs::read_link(debug.join("deps")).unwrap(), foreign);
        // `per-worktree` opts out cleanly around it.
        assert_eq!(
            configure_cargo_target(wt, repo.path(), false).unwrap(),
            Some(wt.join("target"))
        );
        assert_eq!(std::fs::read_link(debug.join("deps")).unwrap(), foreign);
    }

    #[test]
    fn configure_skips_non_cargo_checkout() {
        let dir = TempDir::new().unwrap();
        let wt = dir.path();
        // No Cargo.toml — and a pre-existing `target/debug/build`
        // the lane owns (a Maven-style layout, say). The farm must
        // not create, move or link anything.
        let owned = wt.join("target/debug/build/artifact");
        std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
        std::fs::write(&owned, "maven-out").unwrap();
        let effective = configure_cargo_target(wt, wt, true).unwrap();
        assert_eq!(effective, None);
        assert_eq!(std::fs::read_to_string(&owned).unwrap(), "maven-out");
        assert!(!wt.join("target/debug/deps").exists());
    }

    #[test]
    fn file_lock_probe_releases_a_cloned_holder() {
        let dir = TempDir::new().unwrap();
        let lock = dir.path().join(".cargo-lock");
        std::fs::write(&lock, "").unwrap();
        let holder = std::fs::File::open(&lock).unwrap();
        use std::os::unix::io::AsRawFd;
        assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_EX) }, 0);
        let clone = holder.try_clone().unwrap();
        assert!(file_locked(&lock));
        assert_eq!(unsafe { libc::flock(holder.as_raw_fd(), libc::LOCK_UN) }, 0);
        drop(holder);
        assert!(!file_locked(&lock));
        drop(clone);
    }

    #[test]
    fn configure_refuses_half_shared_when_lock_held() {
        let repo = git_repo();
        let wt = repo.path();
        // A pre-shared lane: real hashed dirs full of artifacts AND a
        // lock file held by a "build". The refusal must come before
        // any merge — every artifact stays in the lane.
        let debug = wt.join("target/debug");
        for name in SHARED_DEBUG_DIRS {
            let dir = debug.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("lane-artifact.rlib"), name).unwrap();
        }
        let lock = debug.join(".cargo-lock");
        std::fs::write(&lock, "").unwrap();
        let f = TestFileLock::acquire(&lock);
        let e = configure_cargo_target(wt, repo.path(), true).unwrap_err();
        assert!(
            e.to_string()
                .contains("retry `issue start` when the lane is idle"),
            "{e}"
        );
        // Nothing linked, nothing moved — the running build's files
        // are exactly where cargo left them.
        for name in SHARED_DEBUG_DIRS.iter().chain(&SHARED_DEBUG_FILES) {
            assert!(!debug.join(name).is_symlink(), "{name}");
        }
        for name in SHARED_DEBUG_DIRS {
            assert_eq!(
                std::fs::read_to_string(debug.join(name).join("lane-artifact.rlib")).unwrap(),
                name,
                "{name} artifacts moved under a held lock"
            );
        }
        assert!(!shared_target_dir(repo.path())
            .join("debug/deps/lane-artifact.rlib")
            .exists());
        f.release();
        // And it plants cleanly once the build is done — artifacts
        // merge into the cache, dirs become links.
        assert_eq!(
            configure_cargo_target(wt, repo.path(), true).unwrap(),
            Some(wt.join("target"))
        );
        assert!(debug.join("deps").is_symlink());
        assert!(shared_target_dir(repo.path())
            .join("debug/deps/lane-artifact.rlib")
            .is_file());
    }

    #[test]
    fn configure_retires_shared_examples_link() {
        let repo = git_repo();
        let wt = repo.path();
        // An r2-era farm: examples linked into the shared cache, with
        // an artifact already in it.
        let debug = wt.join("target/debug");
        let shared_ex = shared_target_dir(repo.path()).join("debug/examples");
        std::fs::create_dir_all(&debug).unwrap();
        std::fs::create_dir_all(&shared_ex).unwrap();
        std::fs::write(shared_ex.join("myexample"), "built").unwrap();
        std::os::unix::fs::symlink(&shared_ex, debug.join("examples")).unwrap();
        configure_cargo_target(wt, repo.path(), true).unwrap();
        // The examples link is gone — a real per-lane dir instead —
        // and its shared content was copied back, not lost.
        assert!(debug.join("examples").is_dir() && !debug.join("examples").is_symlink());
        assert_eq!(
            std::fs::read_to_string(debug.join("examples/myexample")).unwrap(),
            "built"
        );
        // The shared copy stays — another r2-era lane may still link it.
        assert!(shared_ex.join("myexample").is_file());
        assert!(debug.join("deps").is_symlink());
    }

    #[test]
    fn configure_merge_keeps_colliding_artifacts() {
        let repo = git_repo();
        let wt = repo.path();
        // The lane and the cache both carry `dup.rlib` — the merge
        // must not silently delete the lane's copy.
        let debug = wt.join("target/debug");
        let deps = debug.join("deps");
        let shared_deps = shared_target_dir(repo.path()).join("debug/deps");
        std::fs::create_dir_all(&deps).unwrap();
        std::fs::create_dir_all(&shared_deps).unwrap();
        std::fs::write(deps.join("dup.rlib"), "lane's copy").unwrap();
        std::fs::write(deps.join("own.rlib"), "lane only").unwrap();
        std::fs::write(shared_deps.join("dup.rlib"), "shared copy").unwrap();
        configure_cargo_target(wt, repo.path(), true).unwrap();
        assert!(deps.is_symlink());
        // `own.rlib` merged; the collision moved aside, not deleted.
        assert_eq!(
            std::fs::read_to_string(shared_deps.join("own.rlib")).unwrap(),
            "lane only"
        );
        assert_eq!(
            std::fs::read_to_string(shared_deps.join("dup.rlib")).unwrap(),
            "shared copy"
        );
        assert_eq!(
            std::fs::read_to_string(debug.join("deps.local/dup.rlib")).unwrap(),
            "lane's copy"
        );
    }

    #[test]
    fn effective_target_dir_walks_ancestors() {
        let parent = TempDir::new().unwrap();
        let repo = parent.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        // An ancestor's config redirects every lane under it — cargo
        // walks ancestors, so cadence must see the same redirect.
        std::fs::create_dir_all(parent.path().join(".cargo")).unwrap();
        std::fs::write(
            parent.path().join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"shared-out\"\n",
        )
        .unwrap();
        let effective = effective_target_dir(&repo);
        // Relative values resolve against the config's parent dir.
        assert_eq!(effective, parent.path().join("shared-out"));
        // A worktree-local config still wins over the ancestor's.
        std::fs::create_dir_all(repo.join(".cargo")).unwrap();
        std::fs::write(
            repo.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"/mine\"\n",
        )
        .unwrap();
        assert_eq!(effective_target_dir(&repo), PathBuf::from("/mine"));
    }
}
