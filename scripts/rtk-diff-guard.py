#!/usr/bin/env python3
"""rtk-diff-guard — CAD-138: deny `git diff`/`git show` forms the rtk hook rewrites.

The user-level Claude `PreToolUse` hook runs `rtk hook claude`, which rewrites
`git diff` / `git show` (including `git -C/-c ...` spellings and `yadm`) to
`rtk git ...`. rtk's condensed output is not faithful: machine formats like
`--stat`/`--numstat` can print nothing for a non-empty range, so a
net-deletion check reads changed code as clean.

A second hook cannot fix this by racing `updatedInput` (the last hook to
finish wins — nondeterministic). `permissionDecision` aggregates
deterministically instead: deny beats everything, so this hook refuses every
command segment that would execute `git diff`/`git show` through rtk —
whether typed bare and rewritten, or typed as `rtk git ...` directly — and
names the `rtk proxy` form, which the rtk hook never rewrites and which
streams native git output.

Exempt (rtk does not filter these): `rtk proxy ...`, a `\\`-escaped command
word, a segment carrying an `RTK_DISABLED=` assignment (rtk keys on its
presence, not the value), and commands that are not git/rtk at all.

Input: one JSON hook event on stdin; reads `tool_input.command`.
Output: a deny decision on stdout, silence otherwise. Any internal error
exits 0 silently — a broken guard must not break Bash.

`--selftest` runs the deny/allow corpus and exits nonzero on a mismatch.
"""

import json
import re
import shlex
import sys

# rtk's rewrite sees through `command`/`builtin`/`exec`/`sudo` (chainable)
# and `env` (terminal — only VAR=val may follow it). A `-flag` word, a
# `\`-escape, or any other word in command position stops the rewrite; all
# verified against `rtk hook claude` on 0.37.2. `RTK_DISABLED=` in any
# stripped assignment position exempts the command — rtk keys on presence.
CHAIN_WRAPPERS = re.compile(r"^(?:command|builtin|exec|sudo)\b")
ENV_WRAPPER = re.compile(r"^env\b")
ASSIGN = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)=(\S*)")
FLAG = re.compile(r"^-\S+")
# git options that consume the next word as a value.
GIT_VALUE_OPTS = {
    "-c",
    "-C",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
    "--html-path",
    "--man-path",
    "--info-path",
}
FALSIFIED = ("diff", "show")  # rtk's condensed forms for these can print nothing


def segments(command):
    """Split a command line on top-level && || ; | & outside quotes."""
    out, buf, quote, esc = [], [], None, False
    for ch in command:
        if esc:
            buf.append(ch)
            esc = False
        elif ch == "\\" and quote != "'":
            buf.append(ch)
            esc = True
        elif quote:
            buf.append(ch)
            if ch == quote:
                quote = None
        elif ch in "'\"":
            buf.append(ch)
            quote = ch
        elif ch in "&|;":
            out.append("".join(buf))
            buf = []
        else:
            buf.append(ch)
    out.append("".join(buf))
    return out


def git_subcommand(args):
    """First non-option word after `git`; None if there is none."""
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--":
            i += 1
            break
        if a in GIT_VALUE_OPTS:
            i += 2
            continue
        if a.startswith("-"):
            i += 1
            continue
        break
    return args[i] if i < len(args) else None


def filtered_form(segment):
    """Return the offending 'tool subcommand' if the segment would run a
    filtered git diff/show, else None."""
    s = segment.strip()
    disabled = False

    def take_assigns(text):
        nonlocal disabled
        while True:
            m = ASSIGN.match(text)
            if not m:
                return text
            if m.group(1) == "RTK_DISABLED":
                disabled = True
            text = text[m.end() :].lstrip()

    while True:
        s = take_assigns(s)
        if not s or s.startswith("\\") or FLAG.match(s):
            return None  # escaped word, stray flag, or empty — rtk won't touch it
        w = CHAIN_WRAPPERS.match(s)
        if w:
            s = s[w.end() :].lstrip()
            continue
        e = ENV_WRAPPER.match(s)
        if e:  # env is terminal: assigns may follow, then the command word
            s = take_assigns(s[e.end() :].lstrip())
        break
    if disabled:
        return None
    m = re.match(r"^(git|yadm|rtk)\b(.*)", s, re.DOTALL)
    if not m:
        return None
    tool = m.group(1)
    try:
        args = shlex.split(m.group(2))
    except ValueError:
        return None  # unbalanced quotes — not ours to judge
    if tool == "rtk":
        while args and args[0].startswith("-"):  # rtk's own flags
            args = args[1:]
        if not args or args[0] == "proxy":
            return None
        if args[0] in ("git", "yadm"):
            sub = git_subcommand(args[1:])
            return f"rtk {args[0]} {sub}" if sub and sub.startswith(FALSIFIED) else None
        return f"rtk {args[0]}" if args[0].startswith(FALSIFIED) else None
    sub = git_subcommand(args)
    return f"{tool} {sub}" if sub and sub.startswith(FALSIFIED) else None


def proxy_form(seg):
    """The `rtk proxy` spelling of a denied segment: `git diff ...` ->
    `rtk proxy git diff ...`, `rtk git diff ...` -> `rtk proxy git diff ...`,
    `rtk diff ...` -> `rtk proxy git diff ...`."""
    toks = seg.split()
    if toks and toks[0] == "rtk":
        rest = toks[1:]
        if rest and rest[0] in ("git", "yadm"):
            return "rtk proxy " + " ".join(rest)
        return "rtk proxy git " + " ".join(rest)
    return "rtk proxy " + seg


def judge(command):
    """Return (deny_reason, offending_segment) or (None, None)."""
    for seg in segments(command):
        hit = filtered_form(seg)
        if hit:
            seg = seg.strip()
            return (
                f"the rtk hook rewrites `{hit}` and its condensed output can "
                f"print nothing for a real diff — a falsified check (CAD-138). "
                f"Re-run as `{proxy_form(seg)}` to get native git output. "
                f"Treat any filtered empty diff as unproven, not clean.",
                seg,
            )
    return None, None


def selftest():
    deny = [
        "git diff",
        "git diff --stat",
        "git diff origin/main...HEAD --stat",
        "git show HEAD --stat",
        "git -C . diff --stat",
        "git -c core.pager=cat diff",
        "git --no-pager diff origin/main --stat",
        "command git diff origin/main --stat",
        "builtin git diff --stat",
        "env git diff --stat",
        "env GIT_PAGER=cat git diff A B",
        "FOO=bar git diff",
        "sudo git diff",
        "command env git diff",
        "sudo env git diff",
        "command command git diff",
        "exec git diff",
        "A=1 env git diff",
        "env A=1 git diff --stat",
        "git diff && cargo test",
        "cargo test && git show HEAD",
        "cd x && git diff A B --numstat",
        "echo ok; git show",
        "echo RTK_DISABLED=1; git diff",
        "git diff --stat # RTK_DISABLED=1",
        "rtk git diff A B --stat",
        "rtk git show HEAD",
        "rtk diff A B",
        "rtk show HEAD",
        "git show-ref",
        "git difftool",
        "git diff-tree",
        "yadm diff",
        "command rtk git diff",
    ]
    allow = [
        "RTK_DISABLED=0 git diff",
        "RTK_DISABLED= git diff",
        "FOO=1 RTK_DISABLED=0 git diff",
        "sudo RTK_DISABLED=1 git diff",
        "env -i git diff",
        "sudo -u x git diff",
        "exec -a x git diff",
        "command -v git",
        "env command git diff",
        "xargs git diff",
        "nohup git show",
        "time git diff",
        "nice git diff",
        "setsid git diff",
        "watch git diff",
        "ssh host git diff",
        "command \\git diff",
        "FOO=1 \\git diff",
        "git stash show",  # rtk filters it, but it is not a diff/show check — out of scope
        "rtk proxy git diff A B --stat",
        "rtk proxy git show HEAD",
        "rtk proxy",
        "\\git diff --stat",
        "RTK_DISABLED=1 git diff --stat",
        "RTK_DISABLED=true git show",
        "FOO=1 RTK_DISABLED=1 git diff",
        "env rtk proxy git diff",
        "git status",
        "git log --oneline",
        "git commit -m 'diff fix'",
        "git commit -m 'show: summary'",
        "git add -p",
        "git config diff.tool",
        "git -C repo log --oneline",
        "cargo test",
        "echo 'git diff --stat'",
        "gh pr diff",
        "/usr/bin/git diff",  # rtk anchors on a bare `git` word; no rewrite
        "sh -c 'git diff'",   # opaque to the rtk rewrite too
        "cat <<EOF\ngit diff --stat\nEOF",
    ]
    bad = []
    for cmd in deny:
        reason, _ = judge(cmd)
        if reason is None:
            bad.append(f"expected DENY, allowed: {cmd!r}")
    for cmd in allow:
        reason, _ = judge(cmd)
        if reason is not None:
            bad.append(f"expected ALLOW, denied: {cmd!r} ({reason})")
    if bad:
        print("\n".join(bad), file=sys.stderr)
        return 1
    print(f"rtk-diff-guard selftest: {len(deny)} deny + {len(allow)} allow cases pass")
    return 0


def main():
    if "--selftest" in sys.argv[1:]:
        return selftest()
    try:
        event = json.load(sys.stdin)
        command = event.get("tool_input", {}).get("command", "")
        reason, _ = judge(command)
        if reason:
            print(
                json.dumps(
                    {
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "deny",
                            "permissionDecisionReason": reason,
                        }
                    }
                )
            )
    except Exception:
        pass  # a broken guard must not break Bash
    return 0


if __name__ == "__main__":
    sys.exit(main())
