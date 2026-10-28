#!/usr/bin/env python3
"""CAD-1285 acceptance (reviewer-written): the real scripts/check-migration-ddl
must refuse the CAD-1171 pattern -- DDL appended to `app_runs::SCHEMA`, which
the shipped `version < 20` block applies -- and must refuse a later edit that
rides an allow annotation already on the base. Runs in a throwaway detached
worktree of HEAD; works in a depth-1 CI checkout.
"""
import os
import subprocess
import sys
import tempfile

ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True,
                      text=True, check=True).stdout.strip()
ID = ["-c", "user.name=acceptance", "-c", "user.email=acceptance@invalid"]
DECL = 'pub(super) const SCHEMA: &str = "\n'


def git(cwd, *a):
    return subprocess.run(["git", *ID, *a], cwd=cwd, capture_output=True, text=True, check=True).stdout.strip()


def lint(cwd, base):
    r = subprocess.run([sys.executable, os.path.join(cwd, "scripts/check-migration-ddl"), "--base", base],
                       cwd=cwd, capture_output=True, text=True)
    return r.returncode, r.stdout + r.stderr


def edit(path, old, new):
    t = open(path).read()
    assert old in t, f"fixture anchor missing in {path}: {old!r}"
    open(path, "w").write(t.replace(old, new, 1))


def main():
    fails = []
    with tempfile.TemporaryDirectory(prefix="cad1285-") as tmp:
        wt = os.path.join(tmp, "wt")
        git(ROOT, "worktree", "add", "--detach", wt, "HEAD")
        try:
            base = git(wt, "rev-parse", "HEAD")
            rc, out = lint(wt, base)
            if rc != 0:
                fails.append(f"clean tree: want rc=0, got {rc}: {out}")

            # 1. CAD-1171 pattern.
            edit(os.path.join(wt, "src/store/app_runs.rs"), DECL,
                 DECL + "CREATE TABLE IF NOT EXISTS cad1285_probe(id INTEGER PRIMARY KEY);\n")
            git(wt, "commit", "-qam", "cad1171 pattern")
            rc, out = lint(wt, base)
            if rc != 1 or "app_runs::SCHEMA edited" not in out:
                fails.append(f"CAD-1171 pattern: want rc=1 + 'app_runs::SCHEMA edited', got {rc}: {out}")

            # 2. An annotation already on the base must not exempt a later edit.
            git(wt, "reset", "-q", "--hard", base)
            edit(os.path.join(wt, "src/store/schema.rs"), "            if version < 20 {\n",
                 "            // migration-lint: allow 20 -- earlier approved exception\n"
                 "            if version < 20 {\n")
            git(wt, "commit", "-qam", "annotate 20")
            annotated = git(wt, "rev-parse", "HEAD")
            edit(os.path.join(wt, "src/store/app_runs.rs"), DECL,
                 DECL + "CREATE TABLE IF NOT EXISTS cad1285_later(id INTEGER PRIMARY KEY);\n")
            git(wt, "commit", "-qam", "later edit")
            rc, out = lint(wt, annotated)
            if rc != 1 or "app_runs::SCHEMA edited" not in out:
                fails.append(f"inherited annotation: want rc=1 + 'app_runs::SCHEMA edited', got {rc}: {out}")
        finally:
            subprocess.run(["git", "worktree", "remove", "--force", wt], cwd=ROOT, capture_output=True)
    for f in fails:
        print(f"FAIL: {f}")
    print("ok" if not fails else f"{len(fails)} failure(s)")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
