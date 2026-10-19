#!/usr/bin/env python3
"""CAD-1479 INDEPENDENT workflow bad-case acceptance — release-publish-r2 gate.

Authored independently of the implementer; the implementer may not edit
or weaken this file. Amended once, by operator decision: CAD-1506
(2026-10-17) relaxed section 3's absolute "no `environment:`" clause to
"no environment that requires reviewers" — the reasoning is in that
section, and the refusal is preserved.

What the ticket needs: the existing `release-publish-r2` job (the only
writer to the dedicated public-client release store) stays gated so that
it runs only for the real GitHub Release of the approved tag, targets
only the fixed dedicated store, and is an honest skip under a foreign or
absent configuration — never a guessed bucket and never a blind write.

This check exercises the REAL workflow file (.github/workflows/ci.yml)
twice, at the actual gate and the actual run block — not a re-typed copy:

1. CONDITION EVALUATION (real expression semantics): the job's `if:` is
   extracted verbatim from the workflow YAML and evaluated under the
   same operator precedence the Actions expression language defines
   (`&&`/`||`/`==`/`!=`, literals, `vars.*` context). With the fixture
   `vars` at the dedicated store the condition is TRUE; with a foreign
   endpoint, a foreign bucket, an absent config, or a near-miss value
   (subdomain-prefixed host, wrong scheme) it is FALSE — an honest skip.
   The evaluation is a real interpreter for the small expression shape
   the gate uses, so a gate weakened to `!= ''`, a swapped constant or a
   dropped conjunct goes red here.

2. FIXED-STORE SHELL BINDING (real bash): the job's `run:` block is
   extracted verbatim, `${{ ... }}` expressions substituted with the
   fixture context, and the block EXECUTED under `set -euo pipefail` in
   an owned temp dir against an owned `python3`/`gh` stand-in. The
   stand-in records the publisher's argv; the check asserts the real
   invoked arguments name ONLY the dedicated endpoint literal, the
   dedicated bucket literal, `--profile public-client` and
   `--visibility public` — and that every per-target artifact triple
   (archive, .sha256, build bundle) plus the publication receipt and its
   attestation bundle are passed. No expression reaches a shell word
   unquoted for the store binding: the endpoint/bucket/visibility argv
   are literals, so a vars/secrets change can never redirect the store.
   The `gh release view` publishedAt probe runs under the stand-in too
   (it is a metadata read, not a write); a failed probe must not fail
   the publish step (`|| true`), proven by the stand-in exiting nonzero.

3. Dependency gate: the job must declare `needs: release-publish` (the
   upstream job carries the protected `release` environment approval);
   the downstream job may declare only an APPROVAL-FREE environment.
   CAD-1506 (operator decision, 2026-10-17) relaxed the original "no
   `environment:` at all" clause: `release-store` carries a branch policy
   (main and v* tags) with no required reviewers, so it withholds the
   release-store credentials from PR refs without adding a second
   approval. An environment that requires reviewers — the redundant
   approval this clause exists to refuse — still fails here.

4. Preservation: the fixed endpoint/bucket literals are exactly the
   dedicated store CAD-1475 pins (`EXPECTED_ENDPOINT`/`EXPECTED_BUCKET`
   in tests/cad1475_publicity_acceptance.py).

LIMITS: Actions expression evaluation is a faithful local interpreter
for the comparison/boolean subset the gate uses — not the hosted
evaluator; the bash run is the real block with the publisher argv
observed by a stand-in, not a real R2 write. Live GitHub release
contexts, environment protection and real R2 IAM remain CI evidence.
"""
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(
    os.environ.get("CAD1479_REPO", Path(__file__).resolve().parents[1])
)
WORKFLOW = REPO / ".github" / "workflows" / "ci.yml"

# The dedicated release store — identical to the literals
# tests/cad1475_publicity_acceptance.py binds the publicity helper to.
EXPECTED_ENDPOINT = "https://87b59a40e130c425e35474687ad98a4f.r2.cloudflarestorage.com"
EXPECTED_BUCKET = "agenticos-v2-release-artifacts"
JOB = "release-publish-r2"


def workflow_text():
    return WORKFLOW.read_text()


def job_block(name):
    """The named job's whole block: `  <name>:` to the next `  ` job."""
    text = workflow_text()
    m = re.search(rf"^  {re.escape(name)}:\n(.*?)(?=^  \w|\Z)", text, re.M | re.S)
    assert m, f"job {name} must exist in {WORKFLOW.name}"
    return m.group(1)


def job_if(job):
    """The job's `if:` expression, folded (`>` block scalar)."""
    m = re.search(r"^    if: >\n((?:      .*\n)+)", job, re.M)
    if not m:
        m = re.search(r"^    if: (.+)$", job, re.M)
        return m.group(1).strip()
    return " ".join(line.strip() for line in m.group(1).splitlines())


def job_run(job):
    """The publish step's `run:` block (the only multi-line run in the job)."""
    blocks = re.findall(
        r"^        run: \|\n((?:          .*\n)+)", job, re.M
    )
    assert len(blocks) == 1, "the publish job must carry exactly one run block"
    return "\n".join(line[10:] for line in blocks[0].splitlines())


# --- the Actions expression subset interpreter -------------------------------

TOKEN = re.compile(
    r"\s*('(?:[^']|'')*'|==|!=|&&|\|\||\(|\)|[A-Za-z_][A-Za-z0-9_.]*)"
)


def evaluate(expr, context):
    """Evaluate an Actions `if:` subset: equality/boolean over literals and
    context names. Returns bool; raises on anything outside the subset so a
    gate re-shaped into an unknown form is a loud failure, not a pass."""
    tokens = TOKEN.findall(expr)
    assert "".join(tokens) and re.fullmatch(
        r"\s*(?:'(?:[^']|'')*'|==|!=|&&|\|\||\(|\)|[A-Za-z_][A-Za-z0-9_.]*|\s)*\s*",
        expr,
    ), f"expression has a shape outside the check's subset: {expr!r}"
    pos = 0

    def peek():
        return tokens[pos] if pos < len(tokens) else None

    def primary():
        nonlocal pos
        tok = tokens[pos]
        pos += 1
        if tok == "(":
            v = or_()
            assert tokens[pos] == ")", "unbalanced paren"
            pos += 1
            return v
        if tok.startswith("'"):
            return tok[1:-1].replace("''", "'")
        return context.get(tok, "")

    def equality():
        nonlocal pos
        left = primary()
        while peek() in ("==", "!="):
            op = tokens[pos]
            pos += 1
            right = primary()
            left = (left == right) if op == "==" else (left != right)
        return left

    def or_():
        nonlocal pos
        left = and_level()
        while peek() == "||":
            pos += 1
            right = and_level()
            left = bool(left) or bool(right)
        return left

    def and_level():
        nonlocal pos
        left = equality()
        while peek() == "&&":
            pos += 1
            right = equality()
            left = bool(left) and bool(right)
        return left

    result = or_()
    assert pos == len(tokens), f"trailing tokens in {expr!r}"
    return bool(result)


class R2PublishGate(unittest.TestCase):
    def setUp(self):
        self.job = job_block(JOB)
        self.cond = job_if(self.job)

    def context(self, endpoint=EXPECTED_ENDPOINT, bucket=EXPECTED_BUCKET):
        return {
            "vars.RELEASES_R2_ENDPOINT": endpoint,
            "vars.RELEASES_R2_BUCKET": bucket,
        }

    def test_gate_is_true_only_on_the_dedicated_store(self):
        cases = [
            ("dedicated store", self.context(), True),
            ("foreign endpoint", self.context(endpoint="https://evil.example.com"), False),
            ("foreign bucket", self.context(bucket="other-bucket"), False),
            ("absent endpoint", self.context(endpoint=""), False),
            ("absent bucket", self.context(bucket=""), False),
            ("subdomain smuggle",
             self.context(endpoint=EXPECTED_ENDPOINT.replace("https://", "https://evil.")), False),
            ("http scheme", self.context(endpoint=EXPECTED_ENDPOINT.replace("https://", "http://")), False),
            ("both absent", {}, False),
        ]
        for name, ctx, want in cases:
            got = evaluate(self.cond, ctx)
            self.assertEqual(
                got, want,
                f"{name}: gate evaluated {got}, expected {want} (cond: {self.cond!r})",
            )

    # CAD-1506 (operator decision, 2026-10-17): the original clause refused any
    # `environment:` on the publisher, because a protected environment demands a
    # second, redundant approval. The publisher may now declare an APPROVAL-FREE
    # environment: `release-store` has a branch policy (main and v* tags) and no
    # required reviewers, which is what keeps the release-store credentials off
    # PR refs without a second approval on a release. Any other environment is
    # still refused — it would either require reviewers (the redundant approval)
    # or not carry the key.
    APPROVAL_FREE_ENVIRONMENTS = ("release-store",)

    def test_job_needs_the_protected_release_and_no_redundant_approval(self):
        assert re.search(r"^    needs: \[?release-publish\]?[,\]]", self.job, re.M), \
            "the publisher must depend on the release-publish job (the protected release environment's gate)"
        environment = re.search(r"(?m)^    environment: (\S+)$", self.job)
        if environment:
            assert environment.group(1) in self.APPROVAL_FREE_ENVIRONMENTS, (
                "the publisher may declare only an approval-free environment; "
                f"{environment.group(1)!r} would demand a second, redundant approval "
                "or does not carry the release-store credentials")

    def rendered_run(self):
        """The real run block with every ${{ }} expression substituted
        by the fixture context; a leftover/unknown expression fails
        loudly."""
        script = job_run(self.job)
        context = {
            "github.token": "fixture-token",
            "github.ref_name": "v9.9.9",
            "github.sha": "a" * 40,
            "github.repository": "favcrm/cadence",
        }
        def subst(m):
            expr = m.group(1).strip()
            self.assertIn(expr, context,
                          f"unhandled expression in run block: {expr!r}")
            return context[expr]
        script = re.sub(r"\$\{\{\s*([^}]+?)\s*\}\}", subst, script)
        assert "${{" not in script
        return script

    def test_run_block_binds_only_the_dedicated_store(self):
        script = self.rendered_run()
        # The store binding must be literal — no ${{ }} expression may
        # reach the endpoint/bucket/visibility argv at all.
        for flag in ("--endpoint", "--bucket", "--visibility"):
            m = re.search(rf"{re.escape(flag)}\s+\"?([^\"\\\n]+)\"?", script)
            assert m, f"{flag} must appear in the publish invocation"
            arg = m.group(1)
            assert "${{" not in arg and "vars." not in arg, \
                f"{flag} must be a literal, got {arg!r} — an expression here lets a vars change retarget the store"
        assert f'--endpoint "{EXPECTED_ENDPOINT}"' in script
        assert f'--bucket "{EXPECTED_BUCKET}"' in script
        assert '--profile public-client' in script
        assert "--visibility public" in script

        # Execute the REAL run block under owned stand-ins and assert the
        # argv the publisher actually receives.
        with tempfile.TemporaryDirectory(prefix="c1479-wf-") as d:
            d = Path(d)
            tools = d / "tools"
            tools.mkdir()
            dist = d / "dist"
            (dist / "bundles").mkdir(parents=True)
            log = d / "calls.log"
            calls = []
            # Stand-ins: python3 records publish argv; gh release view
            # fails (publishedAt probe must be optional); nothing else.
            (tools / "python3").write_text(
                "#!/bin/sh\necho \"PY:$*\" >> \"$WF_LOG\"\nexit 0\n"
            )
            (tools / "gh").write_text(
                "#!/bin/sh\necho \"GH:$*\" >> \"$WF_LOG\"\nexit 1\n"
            )
            for t in ("python3", "gh"):
                p = tools / t
                p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
            # Seed the dist tree the artifact loop expects.
            for target in ("x86_64-linux", "aarch64-linux", "aarch64-macos"):
                asset = f"cadence-v9.9.9-{target}.tar.gz"
                (dist / asset).write_bytes(b"archive")
                (dist / f"{asset}.sha256").write_text("digest\n")
                (dist / "bundles" / f"build-attestation-{target}.jsonl").write_text("{}\n")
            (dist / "bundles" / "publication-receipt.json").write_text("{}\n")
            (dist / "bundles" / "publication-attestation.jsonl").write_text("{}\n")
            env = dict(os.environ)
            env.update({
                "PATH": f"{tools}:/usr/bin:/bin",
                "WF_LOG": str(log),
                "TAG": "v9.9.9",
                "SOURCE_SHA": "a" * 40,
                "AWS_ACCESS_KEY_ID": "fixture-key",
                "AWS_SECRET_ACCESS_KEY": "fixture-secret",
            })
            out = subprocess.run(
                ["bash", "-c", script], cwd=d, env=env,
                capture_output=True, text=True,
            )
            self.assertEqual(
                out.returncode, 0,
                f"the real publish block must run clean under stand-ins:\n{out.stderr}",
            )
            text = log.read_text()
            calls = [line for line in text.splitlines() if line.startswith("PY:")]
            self.assertEqual(len(calls), 1, "exactly one publish-release.py invocation")
            argv = calls[0][3:]
            assert f'--endpoint {EXPECTED_ENDPOINT}' in argv, argv
            assert f'--bucket {EXPECTED_BUCKET}' in argv, argv
            assert "--profile public-client" in argv, argv
            assert "--visibility public" in argv, argv
            assert "--version v9.9.9" in argv, argv
            # Every artifact triple + the publication pair.
            for target in ("x86_64-linux", "aarch64-linux", "aarch64-macos"):
                for suffix, ctype in ((".tar.gz", "application/octet-stream"),
                                      (".tar.gz.sha256", "text/plain")):
                    name = f"cadence-v9.9.9-{target}{suffix}"
                    assert f"--artifact {name}=dist/{name}:{target}:{ctype}" in argv, argv
                bundle = f"build-attestation-{target}.jsonl"
                assert f"--artifact {bundle}=dist/bundles/{bundle}:{target}:application/json" in argv, argv
            for name in ("publication-receipt.json", "publication-attestation.jsonl"):
                assert f"--artifact {name}=dist/bundles/{name}:publication:application/json" in argv, argv
            # A failed gh probe must not fail the step (|| true on the
            # publishedAt read) — proven by the run's exit 0 above.
            gh_calls = [l for l in text.splitlines() if l.startswith("GH:")]
            assert any("release view" in c for c in gh_calls), gh_calls

    def test_run_block_has_no_writer_without_configured_secrets(self):
        """The real block must refuse before invoking the publisher when
        the access-key env is absent — no blind write, ever."""
        script = self.rendered_run()
        with tempfile.TemporaryDirectory(prefix="c1479-nowrite-") as d:
            d = Path(d)
            tools = d / "tools"
            tools.mkdir()
            log = d / "calls.log"
            (tools / "python3").write_text(
                "#!/bin/sh\necho \"PY:$*\" >> \"$WF_LOG\"\nexit 0\n"
            )
            p = tools / "python3"
            p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
            (d / "dist").mkdir()
            env = dict(os.environ)
            env.update({
                "PATH": f"{tools}:/usr/bin:/bin",
                "WF_LOG": str(log),
                "TAG": "v9.9.9",
                "SOURCE_SHA": "a" * 40,
            })
            env.pop("AWS_ACCESS_KEY_ID", None)
            env.pop("AWS_SECRET_ACCESS_KEY", None)
            out = subprocess.run(
                ["bash", "-c", script], cwd=d, env=env,
                capture_output=True, text=True,
            )
            self.assertNotEqual(out.returncode, 0,
                                "no credentials must fail the step before publishing")
            self.assertFalse(log.exists() and "PY:" in log.read_text(),
                             "the publisher must never run without credentials")


if __name__ == "__main__":
    unittest.main()
