//! Worktree pre-push hook, cargo shim, and lane environment helpers.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::issue::git;

use super::{configure_cargo_target, ensure_cadence_ignored};

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
/// `scripts/boat-build` both resolve per-lane. With `scripts/boat-build` the
/// check plan runs in this worktree's Boat sandbox so the shared host does not
/// compile; a repository without it runs its own `scripts/pre-push` on the
/// host like a git hook — git's arguments, the ref lines on stdin and
/// `CADENCE_PRE_PUSH_MODE=repo` (CAD-2403). The receipt
/// records the checked head, base, sandbox and result under this
/// worktree's git dir. Required CI, review and approval remain
/// authoritative. A `--no-verify` push leaves no receipt, which is the
/// detection signal (contract D3). The hook skips the plan only for the exact
/// pushed HEAD on a clean tree when the last receipt says rc=0 and
/// `tree=clean` for it (CAD-2341); any other receipt runs the plan.
const PRE_PUSH_HOOK: &str = r#"#!/bin/sh
# CAD-1909: run scripts/pre-push in a Boat sandbox through scripts/boat-build;
# the shared host does not compile. A missing Boat CLI refuses the push rather
# than falling back to a local build. CADENCE_PRE_PUSH_LOCAL=1 is the explicit
# emergency path and runs the same plan on this host. CAD-2403: a repository
# without scripts/boat-build runs its own scripts/pre-push here instead, as a
# git hook would (CADENCE_PRE_PUSH_MODE=repo).
top=$(git rev-parse --show-toplevel) || exit 1
gd=$(git rev-parse --absolute-git-dir) || exit 1
cd "$top" || exit 1
# git exports GIT_DIR/GIT_WORK_TREE/etc for the hook — unset them before
# spawning the gate so its own `git` calls (and any subprocess that
# shells out to git, e.g. the contract tests) resolve the worktree normally
# instead of running inside the bare-dir context the hook inherits.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX GIT_QUARANTINE_PATH
tmp="$gd/cadence-pre-push-receipt.tmp"
out="$gd/cadence-pre-push-receipt"
head=$(git rev-parse HEAD 2>/dev/null || echo '?')
lane=${CADENCE_ALIAS:-$USER}
# Git passes one line per ref: local-ref local-sha remote-ref remote-sha.
# Bind the gate to the commit actually being pushed, not merely HEAD, and
# refuse a multi-head push rather than checking only one of them.
pushed=
saw_ref=
refs=
while read -r local_ref local_sha remote_ref remote_sha; do
    saw_ref=1
    refs="$refs$local_ref $local_sha $remote_ref $remote_sha
"
    case "$local_sha" in
        *[!0]*) ;;
        *) continue ;;
    esac
    case " $pushed " in
        *" $local_sha "*) ;;
        *)
            [ -z "$pushed" ] || pushed="multiple"
            [ "$pushed" = multiple ] || pushed="$local_sha"
            ;;
    esac
done
if [ "$pushed" = multiple ]; then
    echo "pre-push: refusing a push of several heads at once; push one branch at a time" >&2
    printf 'head=%s lane=%s mode=boat steps=refused:multiple-heads rc=1 at=%s\n' \
        "$head" "$lane" "$(date +%s)" > "$tmp" 2>/dev/null && mv "$tmp" "$out"
    exit 1
fi
if [ -n "$saw_ref" ] && [ -z "$pushed" ]; then
    printf 'head=%s lane=%s mode=boat steps=skipped:ref-deletion rc=0 at=%s\n' \
        "$head" "$lane" "$(date +%s)" > "$tmp" 2>/dev/null && mv "$tmp" "$out"
    exit 0
fi
[ -n "$pushed" ] || pushed=$head
# A worktree without scripts/pre-push (a fixture, a sparse lane, a repo that
# never carried the script) has no gate to run — record that and let the push
# through rather than blocking on a script that isn't there.
if [ ! -x scripts/pre-push ]; then
    printf 'head=%s lane=%s mode=boat steps=skipped:no-pre-push rc=0 at=%s\n' \
        "$head" "$lane" "$(date +%s)" > "$tmp" 2>/dev/null && mv "$tmp" "$out"
    exit 0
fi
# CAD-2341: GitHub drops an SSH push whose hook runs for minutes (rc=141).
# `scripts/pre-push-check` runs this hook before the push and records the
# receipt. A pass is recorded as `tree=clean` only when the working tree had no
# edits (staged, unstaged or untracked) before and after the plan ran, so it
# describes the commit and not the files beside it. The skip needs all of: no
# PRE_PUSH_TESTS at this push, the pushed commit is HEAD, a clean tree now, and
# a receipt for this head that says rc=0 and tree=clean (a Boat receipt must
# also carry the empty-patch hash). Any other receipt (failure, skipped,
# refused, dirty-recorded) runs the plan again. An unreadable status counts as
# dirty.
EMPTY_PATCH=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
if st=$(git status --porcelain --untracked-files=normal 2>/dev/null) && [ -z "$st" ]; then
    tree_clean=1
else
    tree_clean=
fi
if [ -z "${PRE_PUSH_TESTS:-}" ] && [ "$pushed" = "$head" ] && [ -n "$tree_clean" ] &&
    [ -f "$out" ]; then
    prev=$(tail -n 1 "$out" 2>/dev/null)
    case "$prev" in
        *steps=skipped*|*steps=refused*) ;;
        "head=$head "*" patch=$EMPTY_PATCH "*" rc=0 "*" tree=clean" | "head=$head lane="*" kind="*" rc=0 "*" tree=clean" | "head=$head lane="*" mode=repo rc=0 "*" tree=clean")
            echo "pre-push: recorded clean pass for $head; not re-running the plan"
            exit 0 ;;
    esac
fi
if [ ! -x scripts/boat-build ]; then
    # CAD-2403: a repository without a sandbox helper runs its own
    # scripts/pre-push here, like a git hook: git's remote name and URL as
    # arguments and the ref lines on stdin, plus CADENCE_PRE_PUSH_MODE=repo.
    # It checks the working tree, so the pushed commit must be HEAD. The hook
    # writes the receipt itself.
    commit=$(git rev-parse --verify -q "$pushed^{commit}" 2>/dev/null) || commit=$pushed
    if [ "$commit" != "$head" ]; then
        echo "pre-push: the pushed commit is not HEAD; check out the commit you push and push again" >&2
        printf 'head=%s lane=%s mode=repo steps=refused:head-mismatch rc=1 at=%s\n' \
            "$head" "$lane" "$(date +%s)" > "$tmp" 2>/dev/null && mv "$tmp" "$out"
        exit 1
    fi
    printf '%s' "$refs" | CADENCE_PRE_PUSH_MODE=repo scripts/pre-push "$@"
    rc=$?
    line="head=$head lane=$lane mode=repo rc=$rc at=$(date +%s)"
elif [ "${CADENCE_PRE_PUSH_LOCAL:-}" = 1 ]; then
    if [ "$pushed" != "$head" ]; then
        echo "pre-push: the pushed commit is not HEAD; check out the commit you push and push again" >&2
        printf 'head=%s lane=%s mode=local steps=refused:head-mismatch rc=1 at=%s\n' \
            "$head" "$lane" "$(date +%s)" > "$tmp" 2>/dev/null && mv "$tmp" "$out"
        exit 1
    fi
    output=$(scripts/pre-push --receipt ${PRE_PUSH_TESTS:+--tests})
    rc=$?
    # The plan's progress and failure detail go to stderr; the receipt
    # keeps only its last line.
    printf '%s\n' "$output" >&2
    line=$(printf '%s\n' "$output" | tail -n 1)
else
    output=$(scripts/boat-build --receipt --head "$pushed" ${PRE_PUSH_TESTS:+--tests})
    rc=$?
    line=$(printf '%s\n' "$output" | tail -n 1)
    [ -n "$line" ] || line="head=$pushed lane=$lane mode=boat rc=$rc at=$(date +%s)"
fi
# CAD-2341: a clean pass names the head the plan started on. A commit or
# branch switch during the run (local mode reads HEAD when it ends) leaves
# the receipt unmarked, so the next push runs the plan.
if [ "$rc" -eq 0 ] && [ -n "$tree_clean" ] &&
    [ "$(git rev-parse HEAD 2>/dev/null)" = "$head" ] &&
    case "$line" in "head=$head "*) true ;; *) false ;; esac &&
    st=$(git status --porcelain --untracked-files=normal 2>/dev/null) && [ -z "$st" ]; then
    line="$line tree=clean"
fi
printf '%s\n' "$line" > "$tmp" && mv "$tmp" "$out"
exit $rc
"#;

/// Install the lane's pre-push hook: write `<git-dir>/cadence-hooks/
/// pre-push` (0755) under the worktree's OWN git dir, enable
/// `extensions.worktreeConfig` on the shared config once, then point
/// this worktree's `config.worktree` `core.hooksPath` at that dir.
/// Also sets this worktree's `core.editor` to `true` so commits in the lane
/// never open an interactive editor; an explicit `GIT_EDITOR` still overrides.
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
    // Lane-local noninteractive commit messages: a sentinel or interactive
    // editor must not block `git commit`. An explicit GIT_EDITOR still wins.
    git(wt_dir, &["config", "--worktree", "core.editor", "true"])?;
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

# The absolute admission helper is embedded by the lane generator; do not
# depend on scrubbed CADENCE_* environment or fall back to ambient PATH.
cad=@@CADENCE_BUILD_SLOT@@
"$cad" build-slot run "$slot_kind" -- cargo "$@"
rc=$?
[ "$rc" -eq 0 ] || exit "$rc"
"##;

/// Write the shim under `<git-dir>/cadence-hooks/bin/cargo` (0755) and
/// record its PATH prefix in the worktree's `.env`. Idempotent; a lane
/// minted before slice 3 re-heals on the next `issue start` (the reuse
/// path calls `install_pre_push_hook`, which now calls this).
fn install_cargo_shim(wt_dir: &Path) -> Result<PathBuf> {
    let helper = std::env::current_exe()
        .map_err(|e| Error::internal(format!("could not resolve lane build-slot helper: {e}")))?;
    if !helper.is_absolute() {
        return Err(Error::internal(format!(
            "lane build-slot helper path is not absolute: {}",
            helper.display()
        )));
    }
    let helper = helper.to_str().ok_or_else(|| {
        Error::internal("lane build-slot helper path is not valid UTF-8 for the shell shim")
    })?;
    let cargo_shim = CARGO_SHIM.replace(
        "@@CADENCE_BUILD_SLOT@@",
        &format!("'{}'", helper.replace('\'', "'\\''")),
    );
    let git_dir = PathBuf::from(git(wt_dir, &["rev-parse", "--absolute-git-dir"])?);
    let bin = git_dir.join(HOOKS_DIR).join("bin");
    std::fs::create_dir_all(&bin)?;
    let shim = bin.join("cargo");
    let needs_write = match std::fs::read_to_string(&shim) {
        Ok(existing) => existing != cargo_shim,
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
            f.write_all(cargo_shim.as_bytes())?;
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
    record_lane_env(wt_dir, &text);
    Ok(shim)
}

/// CAD-2290: the file, in the worktree's own git dir, holding the sha256 of
/// the `.env` lane setup last wrote. Hosted finish removes a lane's `.env`
/// only while its content still matches: a line anyone else adds keeps it.
const LANE_ENV_DIGEST: &str = "cadence-lane-env.sha256";

fn lane_env_digest_path(wt_dir: &Path) -> Result<PathBuf> {
    let dir = git(wt_dir, &["rev-parse", "--absolute-git-dir"])?;
    Ok(PathBuf::from(dir.trim()).join(LANE_ENV_DIGEST))
}

/// The lines lane setup itself writes into a lane's `.env`: the cargo-shim
/// `PATH` and the slot env (`write_slot_env`).
const SETUP_ENV_PREFIXES: [&str; 4] = [
    "PATH=",
    "CARGO_BUILD_JOBS=",
    "CADENCE_BUILD_SLOT=",
    "export CADENCE_BUILD_SLOT=",
];

/// Record the digest of `written`, the `.env` setup has just written in
/// `wt_dir`, but only when every line in it is one setup writes. Both writers
/// keep any other line they find; a `.env` carrying one is never recorded as
/// setup's, and an earlier record is dropped. Best effort: without a record,
/// hosted finish keeps refusing the `.env`, which is the safe side, so a
/// failure never fails setup.
pub(crate) fn record_lane_env(wt_dir: &Path, written: &str) {
    let Ok(path) = lane_env_digest_path(wt_dir) else {
        return;
    };
    let setups = written
        .lines()
        .all(|line| SETUP_ENV_PREFIXES.iter().any(|p| line.starts_with(p)));
    if !setups {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let tmp = path.with_extension("sha256.tmp");
    let write = || -> Result<()> {
        std::fs::write(&tmp, crate::delegation::sha256_hex(written.as_bytes()))?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    };
    if write().is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Whether `wt_dir/.env` is a regular file whose content is exactly what lane
/// setup last recorded. No record (a lane from before CAD-2290), a symlink,
/// or any changed byte answers false.
pub(crate) fn lane_env_is_setups(wt_dir: &Path) -> bool {
    let file = wt_dir.join(".env");
    if !std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_file()) {
        return false;
    }
    let Ok(path) = lane_env_digest_path(wt_dir) else {
        return false;
    };
    match (std::fs::read_to_string(path), std::fs::read(&file)) {
        (Ok(recorded), Ok(bytes)) => recorded.trim() == crate::delegation::sha256_hex(&bytes),
        _ => false,
    }
}
