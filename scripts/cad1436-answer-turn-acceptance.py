#!/usr/bin/env python3
"""CAD-1436 isolated cold answering-turn acceptance driver.

Scope: tests configured source guidance delivered through the INITIAL actual
user turn of a fresh managed pi worker on an isolated sandbox daemon. It does
not measure baseline production behaviour.

Supported route (src/cli/launch.rs refuses --instructions-file with
--no-bootstrap for non-codex providers; this driver does not bypass that):
  cadence --state-dir S join <group> pi --model M --effort high --no-bootstrap
          --detach --alias A --cwd D
  cadence --state-dir S message ask A --text <dispatched prompt> --wait N
The dispatched prompt is:
  "Read <owned policy path> for answering policy, then answer this exact question: <Q>"
No bootstrap turn, no separate briefing, no corpus or expected answer in the policy.

Evidence (all from the owned native transcript <state>/agents/<alias>/session.jsonl):
  - the trace starts at the record whose user text equals the dispatched prompt;
  - the first tool call after it must read the policy file successfully;
  - no wiki read may appear before the question prompt;
  - every tool call is paired with exactly one result by id; missing, duplicate or
    orphan ids make the trace unverifiable;
  - a tool result that Pi truncated is used only when its native full-output pointer
    (details.fullOutputPath, also named in the display footer) resolves to an owned
    regular file under the Pi bash-output naming scheme and the display text is
    contained in that file. Nothing else is read.

Wiki shell handling is a small static subset, not a shell interpreter. A command
that mentions cadence/wiki and falls outside the subset is unverifiable (never 0).
Recognised: `cadence wiki search <literal> --json [--path P]` and
`cadence wiki cat <literal-path> [--meta]`, joined by `;`, `&&`, `||`, `|`, newlines,
with stdout redirection (`> file`) and `2>&1`/`2>/dev/null`. Not recognised (so
unverifiable): variables, command substitution, loops, subshells, here-docs,
background jobs, eval/xargs/interpreter wrappers around cadence, non-JSON search,
unknown wiki subcommands.

Gates (reported separately, no overall PASS unless all are pass):
  facts                  required facts in the final answer (question-scoped)
  instruction_read       successful same-turn read of the policy path
  no_prior_source_reads  no wiki read before the question prompt
  citations              every path+rev cited in the final answer is bound to a
                         successful same-turn `cat --meta` object carrying that rev
                         and its body (search hits never bind)
  successful_required_sources  each required source path was read successfully
                         in this turn (currency is NOT asserted here)
  budget                 every wiki search and cat invocation counted (duplicates
                         and --meta included); queries<=3, reads<=8, merged unique
                         candidates<=20; unmeasurable => unverifiable
  trace                  transcript slice found, pairing valid, all wiki calls
                         supported and their evidence complete

Currency (timestamps, approvals, effective dates) is not established by this
driver; answers must state that as an evidence limit.
"""
import argparse
import hashlib
import json
import os
import re
import shlex
import stat
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GOLD = ROOT / 'tests/fixtures/cad1436/gold-cases.json'
SANDBOX_ROOT = Path('/tmp/cad1436/sandbox')
SANDBOX_NAME = 'cad1436'
STATE = SANDBOX_ROOT / SANDBOX_NAME / 'state'
PM_DIR = SANDBOX_ROOT / SANDBOX_NAME / 'pm'
PROD_STATE = Path.home() / '.local/state/cadence'
PROD_PM = Path.home() / 'pm'
PROVIDER = 'pi'
REQUESTED = 'openai-codex/gpt-6-luna'
HISTORICAL_REQUESTED = 'devin/claude-haiku-5-5-high'
GROUP = 'pm1'
RUN_ROOT = Path('/tmp/cad1436/cold-runs')
OFFLINE_ROOT = Path('/tmp/cad1436/offline-eval')
POLICY = Path('/tmp/cad1436/work-pi/answering-policy.md')
POLICY_SHA256 = '4ccb805c4e8ff5ec7d1605d22c964f6be5cf1f4d67ca9dcc2b6398cea0173e46'
ANSWER_WAIT = 600
DEFAULT_CASES = ['A01-infor-checkout-path', 'A08-eva-v1-0-checkin-format',
                 'A14-opera-precheckin-definition']
OPTIN = 'CAD1436_LIVE_TURN'
MAX_QUERIES, MAX_MERGED, MAX_READS = 3, 20, 8

# Native full-output recovery bounds. NATIVE_ROOT is the directory Pi writes
# bash full-output files into; tests may point it at a temporary directory.
NATIVE_ROOT = '/tmp'
NATIVE_MAX_BYTES = 16 * 1024 * 1024
TRUNC_FOOTER_RE = re.compile(r'\[Showing lines [^\n]*?Full output: (/[^\s\]\n]+)\]\s*$')
WIKI_RE = re.compile(r'cadence|wiki')
WRAPPER_ALWAYS = {'eval', 'xargs', 'exec', 'source', '.'}
WRAPPERS = {'bash', 'sh', 'zsh', 'dash', 'python', 'python3', 'node', 'perl', 'ruby',
            'sudo', 'nohup', 'timeout', 'nice', 'time', 'command', 'env', 'script',
            'watch', 'busybox'}

# Per-case required source paths (must be read successfully in the same turn)
# and required facts. Facts are checked by regex on the final answer only.
# A01 is graded against the original ticket acceptance (item 2): method and path,
# the operationId postCheckout, identifier precedence confirmationNumber ->
# account -> roomNumber, and that one of the three checkout flags must be true.
# The question still asks only for method and path; the critical facts are graded
# anyway because the acceptance requires them. Gold file itself is unchanged.
# This is one case of 3; it is not a full gold or ticket PASS.
CASE_SPEC = {
    'A01-infor-checkout-path': {
        'required_facts': [
            (r'(?-i:\bPOST\b)', 'method POST'),
            (r'/hotelOperation/\{property\}/folios/checkout', 'path'),
            (r'\bpostCheckout\b', 'operationId postCheckout'),
            (r'confirmationNumber[\s\S]{0,200}?account[\s\S]{0,200}?roomNumber',
             'identifier precedence confirmationNumber -> account -> roomNumber'),
        ],
        'semantic_facts': [
            (r'\b(?:set\s+)?(?:at\s+least\s+one|one\s+or\s+more|any\s+one|one\s+of)\b'
             r'[^.!?\n]{0,140}\b(?:isCheckOutFolio|isCheckOutWithSettlement|isCheckOutRoom)\b'
             r'[^.!?\n]{0,100}\b(?:is|must\s+be|to)\s+true\b',
             'at least one checkout mode flag stated true'),
        ],
        'forbidden': [r'selfCheckout\s+(is|=)\s+the\s+same',
                      r'\ball\b[\s\S]{0,100}\b(?:checkout\s+)?flags?\b[\s\S]{0,80}\bmust\s+be\s+true\b',
                      r'\ball\b[\s\S]{0,100}\bflags?\b[\s\S]{0,80}\ball\s+required\b',
                      r'\b(?:all|each)\s+(?:three\s+)?(?:checkout\s+)?flags?\s+(?:are\s+)?(?:mandatory|required)\b',
                      r'\bjust\s+one\b[\s\S]{0,80}\binsufficient\b'],
        'ungraded_gold_additional': [],
        'required_sources': ['global/pms/infor/infor-hms-hotel-operation-api-3-8-4-1.md'],
    },
    'A08-eva-v1-0-checkin-format': {
        'required_facts': [
            (r'checkIn', 'field checkIn'),
            (r'checkOut', 'field checkOut'),
            (r'\bString\b', 'type String'),
            (r'\b8\b|eight', 'length 8'),
            (r'mandatory|required', 'mandatory'),
            (r'YYYYMMDD', 'format YYYYMMDD'),
        ],
        'semantic_facts': [],
        'forbidden': [r'timestamp', r'ISO\s*8601'],
        'required_sources': ['global/pms/eva/stb-eva-hotel-interface-v1-0.md'],
    },
    'A14-opera-precheckin-definition': {
        'required_facts': [
            (r'PreCheckin', 'operation'),
            (r'PreRegisterFlag', 'PreRegisterFlag'),
            (r'PreRegisterFlag[^.!?\n]{0,100}(?:xs:boolean|\bboolean\b)|'
             r'(?:xs:boolean|\bboolean\b)[^.!?\n]{0,100}PreRegisterFlag',
             'explicit boolean type associated with PreRegisterFlag'),
            (r'(?:selected|certain|specific|subset)[ -]items?[^.!?\n]{0,120}'
             r'(?:modif(?:y|ied|ication|ies)|updat(?:e|ed|ing))|'
             r'(?:modif(?:y|ied|ication|ies)|updat(?:e|ed|ing))[^.!?\n]{0,120}'
             r'(?:selected|certain|specific|subset)[ -]items?',
             'selected-items modification definition'),
            (r'\b(?:section|sec\.?)[ \t]*31\b|\b(?:PDF[ \t]*)?p(?:age)?\.?[ \t]*885\b',
             'source locator section 31 or PDF page 885'),
        ],
        'semantic_facts': [
            (r'lightweight|modif', 'definition: lightweight modification of selected items'),
            (r'pre-?check-?in', 'pre-check-in purpose'),
        ],
        'forbidden': [r'completes?\s+(the\s+)?(guest\s+)?check-?in'],
        'required_sources': ['global/pms/opera/reservation-web-service-specifications.md'],
    },
}

# Citations in the answer appear as `path` @ rev `fnv1a:...` or path @ rev fnv1a:...
# (backticks and markup may sit between path and rev). Only the path token and the
# rev token are taken; anything between them is markup, not a claim.
CITED_REV_RE = re.compile(r'((?:global|projects|private)/[A-Za-z0-9_./() -]+?\.md)`?\s*@\s*rev\s*`?(fnv1a:[0-9a-f]{6,})')


# ---------------- pure helpers (unit-tested offline) ----------------

def sha_text(s):
    return hashlib.sha256((s or '').encode('utf-8', errors='surrogatepass')).hexdigest()


def dispatched_prompt(policy_path, question):
    """The exact first managed message. Question is included verbatim."""
    return (f"Read {policy_path} for answering policy, then answer this exact "
            f"question: {question}")


def prompt_sha256(text):
    return hashlib.sha256(text.encode()).hexdigest()


def sha256_file(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def user_text(record):
    m = record.get('message') or {}
    if record.get('type') != 'message' or m.get('role') != 'user':
        return None
    c = m.get('content')
    if isinstance(c, str):
        return c
    return ''.join(x.get('text', '') for x in (c or []) if isinstance(x, dict))


def question_slice(recs, prompt):
    """Records from the user record whose text is exactly the dispatched prompt
    to the next user record (or end). Exact match, not substring: the wire text
    is what was dispatched. None when absent (trace unverifiable)."""
    start = None
    for i, r in enumerate(recs):
        if user_text(r) == prompt:
            start = i
            break
    if start is None:
        return None, None
    end = len(recs)
    for j in range(start + 1, len(recs)):
        if user_text(recs[j]) is not None:
            end = j
            break
    return start, recs[start:end]


def pairing_problems(slice_recs):
    """Every tool call needs exactly one result with the same id, and every result
    needs exactly one call. Returns a list of problem strings (empty = valid)."""
    call_n, res_n, problems = {}, {}, []
    for r in slice_recs:
        if r.get('type') != 'message':
            continue
        m = r.get('message') or {}
        if m.get('role') == 'assistant':
            for c in m.get('content') or []:
                if isinstance(c, dict) and c.get('type') == 'toolCall':
                    cid = c.get('id')
                    if not cid:
                        problems.append('tool call without id')
                        continue
                    call_n[cid] = call_n.get(cid, 0) + 1
        elif m.get('role') == 'toolResult':
            cid = m.get('toolCallId')
            if not cid:
                problems.append('tool result without toolCallId')
                continue
            if not isinstance(m.get('content'), list):
                problems.append(f'tool result {cid} has no content list')
            res_n[cid] = res_n.get(cid, 0) + 1
    for cid, n in call_n.items():
        if n > 1:
            problems.append(f'duplicate tool call id {cid}')
        if cid not in res_n:
            problems.append(f'missing tool result for {cid}')
    for cid, n in res_n.items():
        if n > 1:
            problems.append(f'duplicate tool result id {cid}')
        if cid not in call_n:
            problems.append(f'orphan tool result {cid}')
    return problems


def tool_trace(slice_recs):
    """Tool calls (arguments) and results (full display text, details), paired by id, in order.
    A call without a result has result None. Duplicates keep the first occurrence;
    pairing_problems reports them."""
    calls, order, results = {}, [], {}
    for r in slice_recs:
        if r.get('type') != 'message':
            continue
        m = r.get('message') or {}
        if m.get('role') == 'assistant':
            for c in m.get('content') or []:
                if isinstance(c, dict) and c.get('type') == 'toolCall':
                    cid = c.get('id')
                    if cid in calls:
                        continue
                    calls[cid] = {'id': cid, 'tool': c.get('name'),
                                  'arguments': c.get('arguments') or {}}
                    order.append(cid)
        elif m.get('role') == 'toolResult':
            cid = m.get('toolCallId')
            if cid in results:
                continue
            text = ''.join(x.get('text', '') for x in (m.get('content') or [])
                           if isinstance(x, dict))
            results[cid] = {'is_error': bool(m.get('isError')), 'text': text,
                            'text_sha256': sha_text(text),
                            'details': m.get('details') if isinstance(m.get('details'), dict) else None}
    return [{**calls[i], 'result': results.get(i)} for i in order]


def bash_cmd(call):
    return (call.get('arguments') or {}).get('command') if call.get('tool') == 'bash' else None


def _unquoted(cmd):
    return re.sub(r'["\'\\]', '', cmd or '')


class Unsupported(Exception):
    pass


def scan_command(cmd):
    """Split a command into (stage_text, joiner) on unquoted | ; && || and newline.
    Raises Unsupported on constructs outside the static subset."""
    out, buf, i, n, quote = [], [], 0, len(cmd), None
    while i < n:
        ch = cmd[i]
        if quote == "'":
            buf.append(ch)
            if ch == "'":
                quote = None
            i += 1
            continue
        if quote == '"':
            if ch == '\\' and i + 1 < n:
                buf.append(ch)
                buf.append(cmd[i + 1])
                i += 2
                continue
            if ch in '$`':
                raise Unsupported('expansion inside double quotes')
            buf.append(ch)
            if ch == '"':
                quote = None
            i += 1
            continue
        if ch == '\\':
            if i + 1 < n:
                buf.append(ch)
                buf.append(cmd[i + 1])
                i += 2
                continue
            raise Unsupported('trailing backslash')
        if ch in "'\"":
            quote = ch
            buf.append(ch)
            i += 1
            continue
        if ch in '$`(){}<':
            raise Unsupported(f'unsupported shell construct {ch!r}')
        if ch == '&':
            if i + 1 < n and cmd[i + 1] == '&':
                out.append((''.join(buf), '&&'))
                buf = []
                i += 2
                continue
            if buf and buf[-1] == '>':
                buf.append(ch)
                i += 1
                continue
            raise Unsupported('background or unsupported & operator')
        if ch == '|':
            if i + 1 < n and cmd[i + 1] == '|':
                out.append((''.join(buf), '||'))
                buf = []
                i += 2
                continue
            out.append((''.join(buf), '|'))
            buf = []
            i += 1
            continue
        if ch in ';\n':
            out.append((''.join(buf), ';'))
            buf = []
            i += 1
            continue
        buf.append(ch)
        i += 1
    if quote:
        raise Unsupported('unbalanced quote')
    out.append((''.join(buf), ''))
    return out


def pipelines(cmd):
    """Group scanned stages into pipelines (lists of stage texts). Only trailing
    empty statements (e.g. a final newline) are tolerated; any other empty stage is
    a shape the static subset does not accept."""
    stages = scan_command(cmd)
    result, cur = [], []
    for idx, (text, joiner) in enumerate(stages):
        if not text.strip():
            if cur or joiner == '|':
                raise Unsupported('empty pipeline stage')
            if any(t.strip() for t, _ in stages[idx + 1:]):
                raise Unsupported('empty command in sequence')
            break
        cur.append(text)
        if joiner == '|':
            continue
        result.append(cur)
        cur = []
    return result


def parse_stage(text):
    """argv (redirections removed) and whether stdout is redirected to a file."""
    try:
        toks = shlex.split(text, posix=True)
    except ValueError as e:
        raise Unsupported(f'unparseable stage: {e}')
    argv, out_redirect, i = [], False, 0
    while i < len(toks):
        t = toks[i]
        m = re.fullmatch(r'(\d*)(>>?)(&?)(.*)', t)
        if not m:
            argv.append(t)
            i += 1
            continue
        fd, _op, amp, rest = m.groups()
        if amp:
            if fd == '2' and rest == '1':
                pass  # stderr merged into stdout: still visible
            else:
                raise Unsupported(f'unsupported fd duplication {t!r}')
        else:
            if fd == '2':
                # stderr to file/dev-null: not stdout
                if rest == '':
                    i += 1
            elif fd in ('', '1'):
                out_redirect = True
                if rest == '':
                    if i + 1 >= len(toks):
                        raise Unsupported('redirection without target')
                    i += 1
            else:
                raise Unsupported(f'unsupported redirection fd {fd!r}')
        i += 1
    return argv, out_redirect


def parse_cadence(argv):
    """Recognised cadence wiki invocations only."""
    if len(argv) < 3 or argv[1] != 'wiki':
        raise Unsupported('cadence form not recognised')
    sub, rest = argv[2], argv[3:]
    if sub == 'search':
        json_flag, path_opt, pos, i = False, None, [], 0
        while i < len(rest):
            a = rest[i]
            if a == '--json':
                json_flag = True
            elif a == '--path':
                i += 1
                if i >= len(rest):
                    raise Unsupported('search --path without value')
                path_opt = rest[i]
            elif a.startswith('--path='):
                path_opt = a.split('=', 1)[1]
            elif a.startswith('-'):
                raise Unsupported(f'search option {a!r} not recognised')
            else:
                pos.append(a)
            i += 1
        if len(pos) != 1 or not json_flag:
            raise Unsupported('search needs one literal query and --json')
        return {'kind': 'search', 'query': pos[0], 'path_opt': path_opt}
    if sub == 'ls':
        if len(rest) != 1 or '..' in rest[0].split('/') or not re.fullmatch(
                r'[A-Za-z0-9_][A-Za-z0-9_./() -]*', rest[0]):
            raise Unsupported('listing needs exactly one static wiki path')
        # Discovery is neither a search variant nor a body read. Its output
        # cannot prove a source or rescue another invocation's metadata.
        return {'kind': 'discovery', 'path': rest[0]}
    if sub == 'cat':
        meta, pos = False, []
        for a in rest:
            if a == '--meta':
                meta = True
            elif a.startswith('-'):
                raise Unsupported(f'cat option {a!r} not recognised')
            else:
                pos.append(a)
        if len(pos) != 1:
            raise Unsupported('cat needs exactly one literal path')
        p = pos[0]
        if '..' in p.split('/') or not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_./() -]*', p):
            raise Unsupported('cat path is not a static wiki path')
        return {'kind': 'cat', 'path': p, 'meta': meta}
    raise Unsupported(f'wiki subcommand {sub!r} not counted')


# Heads that can execute other code or hide a command: never accepted in a wiki-related command.
EXEC_HEADS = {'sh', 'bash', 'zsh', 'dash', 'ksh', 'eval', 'xargs', 'exec', 'source', '.', 'python',
              'python3', 'node', 'perl', 'ruby', 'busybox', 'script', 'sudo', 'nohup', 'timeout',
              'nice', 'time', 'command', 'watch', 'awk', 'make', 'npm', 'npx'}
# Wrappers that run a command only when given a non-flag argument (bare `env` just prints).
ARG_WRAPPERS = {'env', 'xargs'}
# Read-only stages allowed beside recognised cadence calls. echo/printf may not emit braces
# (no fabricated metadata/search objects); sed may not use its e/w commands.
READONLY_HEADS = {'cd', 'ls', 'which', 'cat', 'head', 'tail', 'grep', 'sed', 'wc', 'sort', 'uniq', 'tr',
                  'true', 'false', 'echo', 'printf', 'env'}
PRODUCER_HEADS = {'echo', 'printf'}
SED_EW_RE = re.compile(r"(^|[\s'\";$/0-9])[ew]([\s'\";}]|$)")


# Inert exit-status token: `$?` inside an echo argument only prints the previous exit code.
# Only the exact quoted forms are normalised; any other `$` shape stays unsupported.
INERT_STATUS_RE = re.compile(r'"(rc|status)=\$\?"')


def normalise_inert_status(cmd):
    return INERT_STATUS_RE.sub(lambda m: f'"{m.group(1)}=0"', cmd or '')


def known_static_invocations(cmd):
    """Count literal wiki calls in shell stages even if a sibling is unsupported.

    This is intentionally only a counter: invocations discovered here never prove
    stdout. Quoted separators are ignored; unknown/dynamic forms remain uncounted
    and the enclosing wiki-related command remains unverifiable.
    """
    stages, buf, quote, escaped, i = [], [], None, False, 0
    while i < len(cmd or ''):
        ch = cmd[i]
        if escaped:
            buf.append(ch)
            escaped = False
            i += 1
            continue
        if quote == "'":
            buf.append(ch)
            if ch == "'":
                quote = None
            i += 1
            continue
        if ch == '\\' and quote != "'":
            buf.append(ch)
            escaped = True
            i += 1
            continue
        if ch in "'\"":
            if quote == ch:
                quote = None
            elif quote is None:
                quote = ch
            buf.append(ch)
            i += 1
            continue
        if quote is None and (ch in ';|\n' or (ch == '&' and i + 1 < len(cmd) and cmd[i + 1] == '&')):
            stages.append(''.join(buf))
            buf = []
            i += 2 if ch in '|&' and i + 1 < len(cmd) and cmd[i + 1] == ch else 1
            continue
        buf.append(ch)
        i += 1
    stages.append(''.join(buf))
    invs = []
    for stage in stages:
        try:
            argv, _redirect = parse_stage(stage)
            if argv[:2] != ['cadence', 'wiki']:
                continue
            inv = parse_cadence(argv)
            invs.append(inv)
        except Unsupported:
            continue
    return invs


def classify_command(cmd):
    """Static classification of a bash command for wiki evidence.

    wiki_related: the command literally mentions cadence or wiki (quotes ignored).
    supported: every stage of a wiki-related command is a recognised cadence wiki call or an
    allowlisted read-only stage, with only `;`, `&&`, `|` joins and no `||`. Anything that could
    hide, fabricate or reorder output (shells, eval, interpreters, env/xargs with arguments,
    brace-bearing echo/printf, `||`) makes the command unsupported. Not a shell interpreter:
    conservative rejection is the intended failure mode.
    """
    if not WIKI_RE.search(_unquoted(cmd)):
        return {'wiki_related': False, 'supported': True, 'reason': '', 'invocations': []}
    cmd = normalise_inert_status(cmd)
    try:
        if any(j == '||' for _t, j in scan_command(cmd)):
            raise Unsupported("'||' conditional can substitute output for a failed wiki call")
        invs = []
        for pl in pipelines(cmd):
            for si, stage in enumerate(pl):
                argv, out_redirect = parse_stage(stage)
                if not argv:
                    raise Unsupported('empty stage')
                head = os.path.basename(argv[0])
                if head in EXEC_HEADS:
                    raise Unsupported(f'{head} can execute hidden commands')
                if head in ARG_WRAPPERS and any(
                        not t.startswith('-') and '=' not in t for t in argv[1:]):
                    raise Unsupported(f'{head} with a command argument')
                if head == 'wiki':
                    raise Unsupported('bare wiki invocation')
                if head == 'cadence':
                    inv = parse_cadence(argv)
                    inv.update(visible=not out_redirect, piped=si < len(pl) - 1)
                    invs.append(inv)
                    continue
                if head not in READONLY_HEADS:
                    raise Unsupported(f'non-allowlisted command {head!r} beside a wiki call')
                if head in PRODUCER_HEADS and '{' in stage:
                    raise Unsupported(f'{head} emits braces: possible fabricated JSON')
                if head == 'sed' and SED_EW_RE.search(' '.join(argv[1:])):
                    raise Unsupported('sed e/w command can execute or write')
        return {'wiki_related': True, 'supported': True, 'reason': '', 'invocations': invs}
    except Unsupported as e:
        return {'wiki_related': True, 'supported': False, 'reason': str(e),
                'invocations': known_static_invocations(cmd)}


def recover_full_output(path, root=None):
    """Read a Pi native full-output file, only if it is an owned regular file
    named like Pi's bash output under the native root. Returns (text, prov) or (None, reason)."""
    root = (root or NATIVE_ROOT).rstrip('/')
    rx = re.compile(re.escape(root) + r'/pi-bash-[0-9a-f]{8,64}\.log')
    if not isinstance(path, str) or not rx.fullmatch(path):
        return None, 'pointer format or location rejected'
    try:
        st = os.lstat(path)
    except OSError as e:
        return None, f'missing or unreadable ({e.__class__.__name__})'
    if stat.S_ISLNK(st.st_mode):
        return None, 'symlink rejected'
    if not stat.S_ISREG(st.st_mode):
        return None, 'not a regular file'
    if st.st_uid != os.getuid():
        return None, 'not owned by this UID'
    if st.st_size > NATIVE_MAX_BYTES:
        return None, 'oversize'
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0))
        with os.fdopen(fd, 'rb') as f:
            data = f.read(NATIVE_MAX_BYTES + 1)
    except OSError as e:
        return None, f'unreadable ({e.__class__.__name__})'
    if len(data) > NATIVE_MAX_BYTES:
        return None, 'oversize'
    if len(data) != st.st_size:
        return None, 'changed during read'
    try:
        text = data.decode('utf-8')
    except UnicodeDecodeError:
        return None, 'not UTF-8'
    return text, {'path': path, 'size': len(data),
                  'sha256': hashlib.sha256(data).hexdigest(), 'owner_uid': st.st_uid}


def complete_result_text(res, call_id):
    """Complete stdout for a paired result, or (None, provenance, reason).
    Recover only via a structured native pointer plus matching footer. Ordinary
    displays must be contained in native output; the exact empty-display UI sentinel
    is accepted only with matching zero-byte truncation metadata and file size."""
    if res is None:
        return None, None, 'no paired tool result'
    text = res.get('text') or ''
    det = res.get('details') if isinstance(res.get('details'), dict) else {}
    prov = {'call_id': call_id, 'display_sha256': sha_text(text), 'mode': 'complete'}
    tr = det.get('truncation')
    fop = det.get('fullOutputPath')
    if not tr:
        if fop or TRUNC_FOOTER_RE.search(text):
            return None, prov, 'truncation footer or pointer without structured metadata'
        return text, prov, None
    if not isinstance(tr, dict):
        return None, prov, 'malformed truncation metadata'
    if not tr.get('truncated'):
        return text, prov, None
    m = TRUNC_FOOTER_RE.search(text)
    if not isinstance(fop, str) or not m or m.group(1) != fop:
        prov['mode'] = 'unverifiable'
        return None, prov, 'truncated result without matching native full-output pointer'
    body = text[:m.start()].rstrip('\n')
    full, fprov = recover_full_output(fop)
    if full is None:
        prov['mode'] = 'unverifiable'
        return None, prov, f'native full output not recoverable: {fprov}'
    if body == '(no output)':
        # This UI sentinel is ambiguous with literal command output. Never let it
        # fall through to ordinary display-containment recovery when its strict
        # zero-byte metadata proof is absent or inconsistent.
        sentinel_metadata = (det.get('fullOutputPath') == m.group(1)
                             and tr.get('content') == ''
                             and tr.get('truncatedBy') == 'bytes'
                             and tr.get('outputLines') == 0
                             and tr.get('outputBytes') == 0
                             and isinstance(tr.get('totalBytes'), int)
                             and not isinstance(tr.get('totalBytes'), bool))
        if not sentinel_metadata or fprov['size'] != tr['totalBytes']:
            prov['mode'] = 'unverifiable'
            return None, prov, 'zero-display sentinel has invalid structured metadata or native size'
        prov.update(mode='recovered', native=fprov,
                    reason='Pi UI zero-display sentinel verified by structured truncation metadata and native file size')
        return full, prov, None
    if body not in full:
        prov['mode'] = 'unverifiable'
        return None, prov, 'display text is not contained in native full output'
    prov.update(mode='recovered', native=fprov,
                reason='Pi display truncated; complete stdout recovered from owned native full-output file')
    return full, prov, None


def parse_objects(text):
    """Semantic parse of complete JSON objects on their own lines.
    search: `{"base"...}` lines (any unparseable one is counted as bad).
    meta: `{"kind":"text",...}` lines with path/rev/text strings (truncated lines are ignored
    here, the caller reports the missing metadata)."""
    search, meta, bad = [], [], 0
    for line in (text or '').split('\n'):
        if line.startswith('{"base"'):
            try:
                obj = json.loads(line)
            except ValueError:
                bad += 1
                continue
            if isinstance(obj, dict):
                search.append(obj)
            else:
                bad += 1
        elif line.startswith('{'):
            try:
                obj = json.loads(line)
            except ValueError:
                continue
            if _is_meta(obj):
                meta.append(obj)
    return search, meta, bad


def _is_meta(obj):
    return (isinstance(obj, dict) and obj.get('kind') == 'text'
            and isinstance(obj.get('path'), str) and isinstance(obj.get('rev'), str)
            and isinstance(obj.get('text'), str))


def meta_header(text, path):
    """The rev a complete metadata object returned for THIS path, or None."""
    _s, meta, _b = parse_objects(text)
    for obj in meta:
        if obj['path'] == path:
            return obj['rev']
    return None


def standalone_call(cmd):
    """The one recognised cadence wiki invocation that IS the whole command, or None.
    No pipe, redirect, sequence, second stage or other command: its stdout is its own."""
    if re.search(r'[<>]', cmd or ''):
        return None
    try:
        stages = pipelines(cmd or '')
        if len(stages) != 1 or len(stages[0]) != 1:
            return None
        argv, _redirect = parse_stage(stages[0][0])
        if argv[:2] != ['cadence', 'wiki']:
            return None
        return parse_cadence(argv)
    except Unsupported:
        return None


def root_object(text):
    """The complete stdout as one JSON object, or None (pretty JSON is valid)."""
    body = (text or '').strip()
    if not body:
        return None
    try:
        obj = json.loads(body)
    except ValueError:
        return None
    return obj if isinstance(obj, dict) else None


def analyze_call(t):
    """Attach 'analysis' to a trace call (once): complete text provenance, wiki
    classification, invocation evidence and problems."""
    if 'analysis' in t:
        return t['analysis']
    a = {'wiki_related': False, 'supported': True, 'reason': '', 'searches': 0,
         'cat_reads': [], 'search_ok': True, 'search_matches': [], 'problems': [],
         'provenance': None}
    t['analysis'] = a
    res = t.get('result')
    text, prov, why = complete_result_text(res, t.get('id'))
    a['provenance'] = prov
    if t.get('tool') != 'bash':
        # Cache/read display views are not source proof and may be UI-clipped.
        return a
    cmd = bash_cmd(t) or ''
    cls = classify_command(cmd)
    a['wiki_related'] = cls['wiki_related']
    if not cls['wiki_related']:
        return a
    a['supported'] = cls['supported']
    a['reason'] = cls['reason']
    if why:
        a['problems'].append(why)
    invs = cls['invocations']
    searches = [i for i in invs if i['kind'] == 'search']
    cats = [i for i in invs if i['kind'] == 'cat']
    a['searches'] = len(searches)
    a['discovery_paths'] = [i['path'] for i in invs if i['kind'] == 'discovery']
    # Count all statically recognized cat attempts before any result/proof exit.
    a['cat_reads'] = [{'path': inv['path'], 'call_id': t.get('id'),
                       'kind': 'meta' if inv['meta'] else 'plain', 'visible': False,
                       'attempted': True, 'ok': False, 'rev': None, 'text': None}
                      for inv in cats]
    if not cls['supported']:
        a['search_ok'] = False
        for rd in a['cat_reads']:
            rd['note'] = 'unsupported compound: counted only, not evidence'
        a['problems'].append(f"unsupported wiki command shape: {cls['reason']}")
        return a
    if res is None:
        a['problems'].append('no paired result for wiki command')
        a['search_ok'] = not searches
        return a
    if res.get('is_error'):
        a['problems'].append('wiki command returned a tool error')
    if text is None:
        a['search_ok'] = not searches
        for rd in a['cat_reads']:
            rd['note'] = 'complete command output unavailable; attempted, not proof'
        return a
    solo = standalone_call(cmd)
    if searches:
        # A search is proof only as the sole command of the bash call, with its complete stdout
        # one root JSON object echoing the query. Shared, piped or redirected output is not.
        obj = root_object(text) if solo and solo['kind'] == 'search' else None
        ms = obj.get('matches') if obj else None
        if not obj or len(searches) != 1 or obj.get('q') != solo['query'] or 'base' not in obj \
                or not isinstance(ms, list):
            a['problems'].append('search is not a standalone complete result (shared, piped or incomplete output)')
            a['search_ok'] = False
        else:
            for mt in ms:
                if isinstance(mt, dict) and isinstance(mt.get('path'), str) and isinstance(mt.get('rev'), str):
                    a['search_matches'].append({'path': mt['path'], 'rev': mt['rev']})
                else:
                    a['problems'].append('search match without path/rev')
                    a['search_ok'] = False
                    break
    for inv, rd in zip(cats, a['cat_reads']):
        rd['visible'] = True
        if not inv['meta']:
            # Plain text is not proof of a successful read of this path: the output may
            # belong to another command in the same stdout. It stays counted and attempted.
            rd['note'] = 'plain read: attempted only, not proof of the path'
            continue
        if solo is None or solo['kind'] != 'cat' or solo['path'] != inv['path'] or res.get('is_error'):
            # Metadata is proof only as the sole command of the bash call: sibling commands
            # (echo, cat, head, sed, pipes) can print a matching JSON line of their own.
            rd['note'] = 'not a standalone --meta call: its stdout cannot be attributed to this path'
            a['problems'].append(f"metadata for {inv['path']} is not a standalone call")
            continue
        obj = root_object(text)
        if obj and _is_meta(obj) and obj['path'] == inv['path'] and obj['text'].strip():
            rd.update(ok=True, rev=obj['rev'], text=obj['text'])
        else:
            a['problems'].append(f"metadata for {inv['path']} is not a complete root object with a body")
    return a


def source_reads(trace):
    """Every wiki cat invocation in the trace with its evidence (ok, rev, body)."""
    reads = []
    for t in trace:
        reads.extend(analyze_call(t)['cat_reads'])
    return reads


WIKI_DOC_RE = re.compile(r'(?<![A-Za-z0-9_/.-])((?:global|projects|private)/[A-Za-z0-9_./() -]+?\.md)\b')


def citations_in_answer(answer):
    """Every (path, rev) pair the final answer cites with `path @ rev fnv1a:...`."""
    return sorted({(m.group(1).strip(), m.group(2)) for m in CITED_REV_RE.finditer(answer or '')})


SENTENCE_BOUNDARY_RE = re.compile(r'(?<=[.;!?])\s+|\n')
NEG_READ_TAIL_RE = re.compile(
    r"(?:\bnot\b|n't\b|\bnever\b)\s+(?:\w+\s+){0,2}?"
    r"(?:read|cross[\s-]?check(?:ed|s)?|check(?:ed|s)?|opened|consulted|inspected|reviewed|verified|used|accessed|looked)"
    r"\b(?:\s+\w+){0,2}[\s`'\"(:]*$", re.I)
POSITIVE_CLAUSE_RE = re.compile(
    r"\b(?:but|however|although|though|while|whereas|and|or|nor|so|because|therefore|thus|which|that|who|if|when|"
    r"is|are|was|were|be|says?|said|states?|stated|shows?|lists?|describes?|confirms?|confirmed|corroborat\w*|"
    r"supports?|supported|matches?|matched|agrees?|indicates?|requires?|means?|gives?|provides?|defines?)\b", re.I)


def _around(text, start, end):
    """Text before and after one occurrence, bounded by its sentence."""
    lo = 0
    for b in SENTENCE_BOUNDARY_RE.finditer(text, 0, start):
        lo = b.end()
    m = SENTENCE_BOUNDARY_RE.search(text, end)
    return text[lo:start], text[end:m.start() if m else len(text)]


def is_nonread_clause(text, start, end):
    """True only for an explicit negative non-read clause that names this one occurrence
    ("I didn't cross-check `P`."). Any positive connective or verb in the same sentence,
    or any other context, keeps the occurrence a bare reference."""
    before, after = _around(text, start, end)
    return bool(NEG_READ_TAIL_RE.search(before)) and not POSITIVE_CLAUSE_RE.search(before + ' ' + after)


def bare_wiki_references(answer):
    """Wiki-doc paths named in the answer that have no emitted path@rev pair. Such a
    reference cannot be bound to a read and makes citations unverifiable, except for an
    occurrence in an explicit non-read clause. A path with any other bare occurrence
    stays flagged."""
    text = answer or ''
    paired = {p for p, _r in citations_in_answer(text)}
    flagged = set()
    for m in WIKI_DOC_RE.finditer(text):
        path = m.group(1).strip()
        if path not in paired and not is_nonread_clause(text, m.start(), m.end()):
            flagged.add(path)
    return sorted(flagged)


def bind_citations(cited, reads):
    """Each emitted (path, rev) must match a successful same-turn read returning it.

    Returns (status, rows). status: pass | unverifiable (no citations or a cited
    pair without same-turn metadata) | fail (a metadata rev differs from answer).
    Primary and secondary citations are treated identically.
    """
    if not cited:
        return 'unverifiable', [{'reason': 'no cited path@rev in final answer'}]
    rows, status = [], 'pass'
    for path, rev in cited:
        ok_reads = [r for r in reads if r['path'] == path and r['ok']]
        returned = {r['rev'] for r in ok_reads} - {None}
        if rev in returned:
            rows.append({'path': path, 'rev': rev, 'bound': True, 'returned_revs': sorted(returned)})
        elif not returned:
            rows.append({'path': path, 'rev': rev, 'bound': False,
                         'reason': 'no successful same-turn read returned metadata for this path'})
            status = 'unverifiable' if status == 'pass' else status
        else:
            rows.append({'path': path, 'rev': rev, 'bound': False,
                         'reason': 'same-turn read returned a different revision',
                         'returned_revs': sorted(returned)})
            status = 'fail'
    return status, rows


def merged_candidates(trace):
    """Unique (path, rev) candidates from complete search results, plus counts.

    Returns (count or None, query_count, bad_calls). Every search invocation is counted,
    duplicates included. Any unsupported wiki command or any search whose evidence is
    incomplete makes the candidate count None (unverifiable).
    """
    queries, bad, seen = 0, 0, set()
    for t in trace:
        a = analyze_call(t)
        if not a['wiki_related']:
            continue
        queries += a['searches']
        if not a['supported']:
            bad += 1
            continue
        if a['searches'] and not a['search_ok']:
            bad += 1
            continue
        for mt in a['search_matches']:
            seen.add((mt['path'], mt['rev']))
    return (None if bad else len(seen)), queries, bad


def grade_facts(case_id, answer):
    """Check marker coverage and known contradictions, never certify semantics.

    Regexes can identify whether a response contains the expected review markers,
    but cannot establish that prose expresses the source-backed proposition.
    Therefore complete marker coverage is still ungraded pending independent review.
    """
    spec = CASE_SPEC.get(case_id) if case_id else None
    if spec is None:
        return 'ungraded', {'marker_coverage': 'not_configured',
                            'marker_missing': [], 'semantic_status': 'ungraded',
                            'provisional_result': 'needs_independent_semantic_review',
                            'forbidden_hits': []}
    text = answer or ''
    semantic_text = re.sub(r'`([^`]*)`', r'\1', text)
    miss = [label for pat, label in spec['required_facts'] if not re.search(pat, text, re.I)]
    smiss = [label for pat, label in spec['semantic_facts'] if not re.search(pat, semantic_text, re.I)]
    forb = [p for p in spec['forbidden'] if re.search(p, text, re.I)]
    coverage = 'complete' if not miss and not smiss else 'incomplete'
    detail = {'marker_coverage': coverage,
              'marker_missing': miss + smiss,
              'semantic_status': 'ungraded',
              'provisional_result': 'needs_independent_semantic_review',
              'forbidden_hits': forb}
    if forb:
        detail['semantic_status'] = 'known_contradiction'
        detail['provisional_result'] = 'fail'
        return 'fail', detail
    return 'ungraded', detail


def budget_gate(queries, merged, reads, unparseable):
    issues = []
    if queries > MAX_QUERIES:
        issues.append(f'queries {queries} > {MAX_QUERIES}')
    if reads > MAX_READS:
        issues.append(f'source reads {reads} > {MAX_READS}')
    if issues:
        return 'fail', {'queries': queries, 'merged': merged, 'source_reads': reads, 'issues': issues}
    if merged is None:
        return 'unverifiable', {'queries': queries, 'merged': None, 'source_reads': reads,
                                'issues': [f'{unparseable} wiki call(s) with unmeasurable evidence']}
    if merged > MAX_MERGED:
        issues.append(f'merged candidates {merged} > {MAX_MERGED}')
        return 'fail', {'queries': queries, 'merged': merged, 'source_reads': reads, 'issues': issues}
    return 'pass', {'queries': queries, 'merged': merged, 'source_reads': reads, 'issues': []}


def cleanup_residue(aliases_after):
    """Created aliases still in the registry after cleanup (must be empty)."""
    return sorted(aliases_after or [])


def invalid_case_ids(ids, by_id, capture_only=False):
    return [i for i in ids if i not in by_id or (i not in CASE_SPEC and not capture_only)]


def analyze_turn(case_id, recs, prompt, answer, turn_status, transcript=None):
    """Grade one question turn from its owned transcript records and the final answer.
    Pure: no daemon calls. Used by the live run and by offline re-evaluation."""
    start, sl = question_slice(recs, prompt)
    if sl is None:
        return {'overall': 'trace_unverifiable', 'reason': 'dispatched prompt not found as an exact user record',
                'gates': {}}
    prior_recs = recs[:start]
    prior = tool_trace(prior_recs)
    prior_problems = pairing_problems(prior_recs)
    trace = tool_trace(sl)
    trace_problems = list(pairing_problems(sl))
    for t in trace:
        for p in analyze_call(t)['problems']:
            trace_problems.append(f"{t['id']}: {p}")
    trace_ok = not trace_problems
    gates = {'trace': 'pass' if trace_ok else 'unverifiable'}
    prior_wiki = [t for t in prior if bash_cmd(t) and WIKI_RE.search(_unquoted(bash_cmd(t)))]
    gates['no_prior_source_reads'] = ('unverifiable' if prior_problems
                                      else 'fail' if prior_wiki else 'pass')
    first = trace[0] if trace else None
    first_res = first.get('result') if first else None
    instruction_ok = (first is not None and first.get('tool') == 'read'
                      and (first.get('arguments') or {}).get('path') == str(POLICY)
                      and first_res and not first_res['is_error'] and first_res['text']
                      and not first['analysis']['problems'])
    instr = [first] if instruction_ok else []
    gates['instruction_read'] = 'pass' if instruction_ok else 'ungraded'
    fstat, fdet = grade_facts(case_id, answer or '')
    gates['facts'] = fstat
    cited = citations_in_answer(answer or '')
    reads = source_reads(trace)
    cstat, crows = bind_citations(cited, reads)
    bare = bare_wiki_references(answer or '')
    if bare:
        crows = crows + [{'path': p, 'bound': False,
                          'reason': 'wiki-doc path named in the answer without an emitted path@rev'}
                         for p in bare]
        if cstat == 'pass':
            cstat = 'unverifiable'
    gates['citations'] = cstat
    spec = CASE_SPEC.get(case_id) if case_id else None
    req = spec['required_sources'] if spec else None
    # Proof: a visible metadata read of the required path in this turn, with a complete
    # object and a nonempty body, whose rev is the one the final answer emits for that path.
    emitted = {path: rev for path, rev in cited}
    proven = {r['path'] for r in reads if r['ok'] and r['kind'] == 'meta'}
    missing_req = ([p for p in req if p not in proven or emitted.get(p) != next(
        (r['rev'] for r in reads if r['path'] == p and r['ok'] and r['kind'] == 'meta'), None)]
        if req is not None else [])
    gates['successful_required_sources'] = ('ungraded' if req is None else
                                            'pass' if not missing_req else 'unverifiable')
    if req is None:
        src_problems = []
    elif missing_req:
        src_problems = [f'required source lacks proven same-turn metadata or emitted citation: {p}'
                        for p in missing_req]
    else:
        src_problems = []
    merged, queries, bad = merged_candidates(trace)
    reads_n = sum(len(analyze_call(t)['cat_reads']) for t in trace)
    bstat, bdet = budget_gate(queries, merged, reads_n, bad)
    gates['budget'] = bstat
    if turn_status != 'completed':
        overall, reason = 'failed', f'turn status {turn_status!r}'
    elif any(gates[k] == 'fail' for k in ('facts', 'citations', 'budget', 'no_prior_source_reads')):
        overall, reason = 'fail', 'graded'
    elif all(v == 'pass' for v in gates.values()):
        overall, reason = 'pass', 'graded'
    elif any(v == 'unverifiable' for v in gates.values()):
        probs = sorted({p for p in trace_problems + prior_problems + src_problems})
        overall, reason = 'unverifiable', 'evidence incomplete: ' + ('; '.join(probs) if probs else
                                                                    ', '.join(k for k, v in gates.items() if v == 'unverifiable'))
    else:
        overall, reason = 'ungraded', 'graded'
    return {
        'overall': overall, 'reason': reason, 'gates': gates,
        'facts_detail': fdet, 'citation_rows': crows, 'budget': bdet,
        'trace': {'status': 'verified' if trace_ok else 'unverifiable', 'transcript': transcript,
                  'question_record_index': start, 'records_in_case': len(sl),
                  'prior_tool_calls': len(prior), 'prior_problems': prior_problems,
                  'problems': trace_problems, 'tool_calls': trace},
        'instruction_read_calls': [c['id'] for c in instr],
        'provenance': [analyze_call(t)['provenance'] for t in trace if analyze_call(t)['provenance']],
        'source_reads': [{k: r[k] for k in ('path', 'call_id', 'kind', 'visible', 'ok', 'rev')} for r in reads],
    }


# ---------------- daemon I/O ----------------

def cad(*args, timeout=240):
    argv = ['cadence', '--state-dir', str(STATE), *args]
    try:
        p = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
        return {'argv': argv, 'exit': p.returncode, 'stdout': p.stdout, 'stderr': p.stderr}
    except (OSError, subprocess.TimeoutExpired) as e:
        return {'argv': argv, 'exit': None, 'stdout': '', 'stderr': str(e)}


def jparse(text):
    try:
        return json.loads(text)
    except (ValueError, TypeError):
        return None


def under(p, parent):
    try:
        Path(p).resolve().relative_to(Path(parent).resolve())
        return True
    except ValueError:
        return False


def chk(name, ok, detail, required=True):
    return {'check': name, 'ok': bool(ok), 'required': required, 'detail': detail}


def agent_aliases():
    r = cad('agent', 'list', '--all', timeout=60)
    d = jparse(r['stdout']) if r['exit'] == 0 else None
    return None if d is None else [a['alias'] for a in d.get('agents', [])]


def pm_yaml_models():
    import yaml
    cfg = yaml.safe_load((PM_DIR / 'pm.yaml').read_text()) or {}
    models = ((cfg.get('pi') or {}).get('models') or {})
    return {'allow': models.get('allow') or [], 'worker_allow': models.get('worker_allow') or [],
            'default_worker': (models.get('default') or {}).get('worker')}


def preflight():
    out = [chk('sandbox root under /tmp', under(SANDBOX_ROOT, '/tmp'), str(SANDBOX_ROOT)),
           chk('sandbox state/pm are not production',
               not (under(STATE, PROD_STATE) or under(PM_DIR, PROD_PM)),
               {'state': str(STATE), 'pm': str(PM_DIR)})]
    st = subprocess.run(['cadence', '--org', 'local', 'dev', 'status', SANDBOX_NAME],
                        capture_output=True, text=True, timeout=60,
                        env={**os.environ, 'CADENCE_SANDBOX_ROOT': str(SANDBOX_ROOT)})
    body = st.stdout[st.stdout.find('{'):] if '{' in st.stdout else ''
    sd = jparse(body) if st.returncode == 0 else None
    out.append(chk('isolated daemon running', bool(sd and sd.get('daemon') == 'running'),
                   sd or {'exit': st.returncode}))
    doc = cad('doctor', timeout=120)
    d = jparse(doc['stdout']) if doc['exit'] == 0 else None
    out.append(chk('pi CLI present', bool(d and d.get('checks', {}).get('pi', {}).get('present')),
                   (d or {}).get('checks', {}).get('pi', {'exit': doc['exit']})))
    try:
        m = pm_yaml_models()
    except (OSError, ValueError) as e:
        m = {'error': str(e)}
    listed = REQUESTED in (m.get('allow') or []) and REQUESTED in (m.get('worker_allow') or [])
    out.append(chk('requested model in sandbox pm.yaml allow AND worker_allow', listed,
                   {'requested': REQUESTED, **m}))
    al = agent_aliases()
    out.append(chk('pm group root exists', al is not None and GROUP in al, {'group': GROUP, 'aliases': al}))
    out.append(chk('policy file hash matches recorded value',
                   POLICY.exists() and sha256_file(POLICY) == POLICY_SHA256,
                   {'path': str(POLICY), 'sha256': sha256_file(POLICY) if POLICY.exists() else None}))
    return out


def unique_alias(run_id, case_id):
    short = re.sub(r'[^a-z0-9]', '', case_id.lower())[:8]
    return f'c{run_id}-{short}'[:60]


def transcript_path(alias):
    return STATE / 'agents' / alias / 'session.jsonl'


def read_transcript(alias):
    p = transcript_path(alias)
    if not p.exists():
        return None, f'owned transcript missing: {p}'
    try:
        return [json.loads(l) for l in p.read_text().splitlines() if l.strip()], None
    except (OSError, ValueError) as e:
        return None, f'owned transcript unreadable: {e}'


def worker_record(alias):
    s = cad('agent', 'show', alias, timeout=60)
    body = jparse(s['stdout']) if s['exit'] == 0 else None
    a = (body or {}).get('agent') or {}
    return {'exit': s['exit'], 'alias': alias, 'state': a.get('state'),
            'session_id': a.get('session_id') or a.get('thread_id'),
            'model_configured': a.get('model_configured'), 'model_effective': a.get('model_effective'),
            'model_source': a.get('model_source'), 'model_reported': a.get('model_reported'),
            'effort_effective': a.get('effort_effective')}


def run_case(case, run_id, run_dir, created, capture_only=False):
    rec = {'case': case['id'], 'question': case['question'], 'requested_model': REQUESTED}
    alias = unique_alias(run_id, case['id'])
    cwd = run_dir / 'cwd' / case['id']
    cwd.mkdir(parents=True, exist_ok=True)
    prompt = dispatched_prompt(str(POLICY), case['question'])
    rec['dispatched_prompt'] = prompt
    rec['dispatched_prompt_sha256'] = prompt_sha256(prompt)
    rec['policy_path'] = str(POLICY)
    rec['policy_sha256'] = sha256_file(POLICY)
    rec['alias'] = alias
    if alias in (agent_aliases() or []):
        rec['overall'] = 'blocked'
        rec['reason'] = f'alias {alias} already exists; refusing to reuse'
        return rec
    j = cad('join', GROUP, PROVIDER, '--model', REQUESTED, '--effort', 'high', '--no-bootstrap',
            '--detach', '--alias', alias, '--cwd', str(cwd), timeout=900)
    rec['join_exit'] = j['exit']
    if j['exit'] != 0:
        rec['overall'] = 'blocked'
        rec['reason'] = f"join refused: {j['stderr'][-300:] or j['stdout'][-300:]}"
        return rec
    created.append(alias)
    try:
        cad('agent', 'wait', alias, '--timeout', '600', timeout=660)
        eff = worker_record(alias)
        rec['effective'] = eff
        rec['session_id'] = eff.get('session_id')
        selected = {key: eff.get(key) for key in ('model_configured', 'model_effective', 'model_reported')}
        if any(selected[key] != REQUESTED for key in selected):
            rec['overall'] = 'blocked'
            rec['reason'] = f"configured/effective/reported model must all be {REQUESTED!r}: {selected!r}"
            return rec
        ask = cad('message', 'ask', alias, '--text', prompt, '--wait', str(ANSWER_WAIT),
                  timeout=ANSWER_WAIT + 120)
        rec['ask_exit'] = ask['exit']
        msg = jparse(ask['stdout']) if ask['exit'] in (0, 2) else None
        if msg is None:
            rec['overall'] = 'failed'
            rec['reason'] = f"message ask produced no record (exit {ask['exit']})"
            return rec
        result = msg.get('result') or {}
        rec['message_id'] = msg.get('id')
        rec['turn_id'] = msg.get('turn_id')
        rec['turn_status'] = result.get('status')
        rec['answer'] = result.get('text')
        recs, err = read_transcript(alias)
        if recs is None:
            rec['trace'] = {'status': 'trace_unverifiable', 'reason': err}
            rec['overall'] = 'trace_unverifiable'
            return rec
        spec_id = case['id'] if case['id'] in CASE_SPEC else None
        ana = analyze_turn(spec_id, recs, prompt, rec['answer'] or '', rec['turn_status'],
                           transcript=str(transcript_path(alias)))
        rec.update(ana)
        if capture_only:
            rec['capture_only'] = True
            if rec.get('overall') == 'pass':
                rec['overall'] = 'ungraded'
                rec['reason'] = 'capture-only evidence; independent semantic review required'
        return rec
    finally:
        stop = cad('agent', 'stop', alias, timeout=120)
        rem = cad('agent', 'remove', alias, timeout=120)
        rec['cleanup'] = {'stop': stop['exit'], 'remove': rem['exit']}
        if stop['exit'] == 0 and rem['exit'] == 0:
            created.remove(alias)


def offline_eval(report_path):
    """Re-grade an existing run offline with this driver. No daemon calls, no new turns.
    Writes a separate hash-bound evaluation; the original report is not modified."""
    rp = Path(report_path).resolve()
    rep_sha_before = sha256_file(rp)
    rep = json.loads(rp.read_text())
    gold_sha = sha256_file(GOLD)
    stamp = datetime.now(timezone.utc).strftime('%Y%m%d%H%M%S')
    out = OFFLINE_ROOT / f'eval-{stamp}'
    (out / 'native').mkdir(parents=True, exist_ok=False)
    cases, native_hashes = [], {}
    for c in rep.get('cases', []):
        row = {'case': c.get('case'), 'alias': c.get('alias'), 'original_overall': c.get('overall'),
               'original_gates': c.get('gates')}
        if not c.get('alias') or not c.get('dispatched_prompt'):
            row.update(offline_overall='not-evaluable', reason='report lacks alias or dispatched prompt')
            cases.append(row)
            continue
        tp = transcript_path(c['alias'])
        if not under(tp, STATE) or not tp.exists():
            row.update(offline_overall='not-evaluable', reason='owned transcript missing or outside sandbox state')
            cases.append(row)
            continue
        raw = tp.read_bytes()
        recs = [json.loads(l) for l in raw.decode().splitlines() if l.strip()]
        ana = analyze_turn(c['case'], recs, c['dispatched_prompt'], c.get('answer') or '',
                           c.get('turn_status'), transcript=str(tp))
        for p in ana.get('provenance') or []:
            if p.get('mode') == 'recovered':
                nat = p['native']
                data, nprov = recover_full_output(nat['path'])
                if data is None or nprov['sha256'] != nat['sha256']:
                    raise SystemExit(f'native artifact changed or unreadable: {nat["path"]}')
                dst = out / 'native' / f"{nat['sha256']}.log"
                dst.write_bytes(data.encode('utf-8'))
                native_hashes[nat['path']] = nat['sha256']
        row.update(transcript={'path': str(tp), 'sha256': hashlib.sha256(raw).hexdigest()},
                   dispatched_prompt_sha256=c.get('dispatched_prompt_sha256'),
                   offline_overall=ana['overall'], offline_reason=ana.get('reason'),
                   offline_gates=ana.get('gates'), offline=ana)
        cases.append(row)
    if sha256_file(rp) != rep_sha_before:
        raise SystemExit('original report changed during offline evaluation')
    evaluation = {
        'ticket': 'CAD-1436', 'kind': 'offline re-evaluation of an existing run (no new runtime check)',
        'at': datetime.now(timezone.utc).isoformat(),
        'driver': {'path': str(Path(__file__).resolve()), 'sha256': sha256_file(__file__)},
        'tests': {'path': str((Path(__file__).parent / 'test_cad1436_driver.py').resolve()),
                  'sha256': sha256_file(Path(__file__).parent / 'test_cad1436_driver.py')},
        'gold': {'path': str(GOLD), 'sha256': gold_sha},
        'original_report': {'path': str(rp), 'sha256': rep_sha_before, 'verdict': rep.get('verdict')},
        'native_artifacts': [{'source': k, 'sha256': v, 'copy': str(out / 'native' / f'{v}.log')}
                             for k, v in native_hashes.items()],
        'cases': cases,
        'not_claimed': ['no overall PASS', 'no independent PASS', 'no full 30-case acceptance',
                        'no member/security/revocation/sleep-wake evidence'],
    }
    (out / 'evaluation.json').write_text(json.dumps(evaluation, indent=2))
    print(json.dumps({'evaluation': str(out / 'evaluation.json'),
                      'driver_sha256': evaluation['driver']['sha256'],
                      'original_report_sha256': rep_sha_before,
                      'case_offline_overall': {c['case']: c.get('offline_overall') for c in cases}}, indent=2))
    return 0


def main():
    ap = argparse.ArgumentParser()
    m = ap.add_mutually_exclusive_group()
    m.add_argument('--check', action='store_true')
    m.add_argument('--run', action='store_true')
    m.add_argument('--offline-eval', metavar='REPORT', default=None)
    ap.add_argument('--cases', default=None)
    ap.add_argument('--capture-only', action='store_true',
                    help='capture non-frozen case turns without configured fact/source assertions')
    args = ap.parse_args()
    if args.offline_eval:
        return offline_eval(args.offline_eval)
    mode = 'run' if args.run else 'check'
    if args.capture_only and not args.run:
        ap.error('--capture-only requires --run')
    gold = json.loads(GOLD.read_text())
    ids = args.cases.split(',') if args.cases else DEFAULT_CASES
    by_id = {c['id']: c for c in gold['cases']}
    bad = invalid_case_ids(ids, by_id, args.capture_only)
    if bad:
        print(json.dumps({'error': f'unknown or ungraded case ids {bad}'}))
        return 2
    run_id = datetime.now(timezone.utc).strftime('%m%d%H%M%S')
    run_dir = RUN_ROOT / f'run-{run_id}'
    run_dir.mkdir(parents=True, exist_ok=True)
    pre = preflight()
    pre_ok = all(c['ok'] for c in pre if c['required'])
    report = {'ticket': 'CAD-1436', 'mode': mode, 'capture_only': args.capture_only,
              'run_id': run_id, 'requested': REQUESTED,
              'historical_requested_model': HISTORICAL_REQUESTED,
              'at': datetime.now(timezone.utc).isoformat(),
              'scope': 'configured source guidance via the initial user turn of a fresh no-bootstrap managed pi worker (isolated sandbox); not baseline production behaviour',
              'policy': {'path': str(POLICY), 'sha256': sha256_file(POLICY) if POLICY.exists() else None,
                         'source': 'skills/cadence/SKILL.md Business knowledge + agents/master/AGENT.md wiki bullet'},
              'preflight': pre, 'preflight_ok': pre_ok,
              'member_gate': {'status': 'not-run', 'reason': 'no separate ordinary-member identity (PMS-10)'},
              'cases': []}
    created = []
    if mode == 'check':
        report['verdict'] = 'preflight-ok' if pre_ok else 'preflight-blocked'
    elif os.environ.get(OPTIN) != '1':
        report['verdict'] = 'refused: CAD1436_LIVE_TURN=1 not set'
        report['cases'] = [{'case': i, 'overall': 'not-run'} for i in ids]
    elif not pre_ok:
        report['verdict'] = 'blocked: preflight failed'
        report['cases'] = [{'case': i, 'overall': 'blocked'} for i in ids]
    else:
        try:
            report['cases'] = [run_case(by_id[i], run_id, run_dir, created, args.capture_only) for i in ids]
        finally:
            # Residue check: every created alias must be absent from the registry.
            after = agent_aliases()
            residue = cleanup_residue([a for a in created if after is None or a in after])
            report['cleanup_residue'] = residue
            if residue:
                (run_dir / 'manifest-retained.json').write_text(json.dumps({'residue': residue}, indent=2))
        ov = [c.get('overall') for c in report['cases']]
        report['verdict'] = ('capture-only: semantic review required' if args.capture_only else
                             'pass' if ov and all(o == 'pass' for o in ov) and not report['cleanup_residue']
                             else 'not-pass')
        report['session_ids'] = [c.get('session_id') for c in report['cases']]
        report['sessions_distinct'] = len(set(report['session_ids'])) == len(report['session_ids'])
    (run_dir / 'report.json').write_text(json.dumps(report, indent=2))
    print(json.dumps({'report': str(run_dir / 'report.json'), 'mode': mode,
                      'verdict': report.get('verdict'), 'preflight_ok': pre_ok,
                      'cleanup_residue': report.get('cleanup_residue'),
                      'case_overall': {c['case']: c.get('overall') for c in report.get('cases', [])}}, indent=2))
    if mode == 'check':
        return 0 if pre_ok else 1
    return 0 if report.get('verdict') == 'pass' else 1


if __name__ == '__main__':
    sys.exit(main())
