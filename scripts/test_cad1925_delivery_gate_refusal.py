#!/usr/bin/env python3
"""CAD-1925 reviewer-owned refusal acceptance, fixed from the kickoff/ticket.

Run from the candidate repository root:
    python3 scripts/test_cad1925_delivery_gate_refusal.py
The pre-commit copy can also be run by its absolute /tmp path from that root.

Real delivery-gate subprocesses and real enqueue-reviewed main/evaluate run.
Only enqueue's existing external gh/git/cadence transports are synthetic. No
real enqueue, comment, approval or other network write is permitted. Signatures
are independently made/checked with OpenSSH over the SPECIFIED payload, not a
copy of the implementation. Keys, HOME, GH configuration and TMPDIR are private
throwaway fixtures. Missing implementation/prerequisites FAIL, never skip.

This proves the local merge-group verifier's refusal, not live GitHub branch
protection activation. The workflow check is static, not a YAML interpreter or
proof against a PR editing that workflow. A same-uid reader of the real private
key can still sign (the design's explicit CAD-280 residual); this check does not
claim otherwise. Implementer must commit this reviewer-owned file unchanged.
"""

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import types
import unittest
from unittest import mock


ROOT = Path.cwd().resolve()
GATE = ROOT / "scripts/delivery-gate"
ENQUEUE = ROOT / "scripts/enqueue-reviewed"
REPO = "example/cadence"
PR = 73
HEAD = "a" * 40
MARKER = "<!-- cadence-delivery-gate v1 -->"
NAMESPACE = PRINCIPAL = "cadence-delivery-gate"
REAL_RUN = subprocess.run


def payload(repo=REPO, pr=PR, head=HEAD):
    # Exact bytes prescribed by the independent kickoff, NOT comment text.
    return f"cadence-delivery-gate v1\nrepo {repo}\npr {pr}\nhead {head}\n"


def comment(signature, repo=REPO, pr=PR, head=HEAD):
    return f"{MARKER}\nDelivery evidence for {repo}#{pr}, head {head}.\n\n{signature}"


class Sandbox(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cad1925-refusal-")
        self.addCleanup(self.temp.cleanup)
        self.tmp = Path(self.temp.name)
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.gh_attempts = self.tmp / "unexpected-gh-invocations"
        ssh = shutil.which("ssh-keygen")
        self.assertIsNotNone(ssh, "OpenSSH ssh-keygen -Y is a required prerequisite")
        self.ssh = str(Path(ssh).resolve())
        (self.bin / "ssh-keygen").symlink_to(self.ssh)
        (self.bin / "python3").symlink_to(sys.executable)
        # Defense in depth if the SUT accidentally bypasses its run transport.
        # This gh NEVER delegates; it records locally and refuses all commands.
        poison = self.bin / "gh"
        poison.write_text(
            f"#!{sys.executable}\nimport pathlib,sys\n"
            f"p=pathlib.Path({str(self.gh_attempts)!r})\n"
            "with p.open('a') as f: f.write(repr(sys.argv[1:])+'\\n')\n"
            "raise SystemExit(99)\n", encoding="utf-8")
        poison.chmod(0o700)
        self.env = os.environ.copy()
        for name in ("GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN",
                     "GITHUB_ENTERPRISE_TOKEN", "CADENCE_DELIVERY_GATE_KEY",
                     "ENQ_SCRIPT", "SSH_AUTH_SOCK", "SSH_ASKPASS", "SSH_ASKPASS_REQUIRE"):
            self.env.pop(name, None)
        for name, directory in {
            "HOME": "home", "GH_CONFIG_DIR": "gh-config", "TMPDIR": "tmp",
            "XDG_CONFIG_HOME": "xdg-config", "XDG_DATA_HOME": "xdg-data",
            "XDG_STATE_HOME": "xdg-state", "XDG_CACHE_HOME": "xdg-cache",
        }.items():
            path = self.tmp / directory
            path.mkdir()
            self.env[name] = str(path)
        self.env.update(PATH=str(self.bin), PYTHONDONTWRITEBYTECODE="1")
        self.key = self.tmp / "trusted-key"
        self.other_key = self.tmp / "untrusted-key"
        for key in (self.key, self.other_key):
            result = self.proc([self.ssh, "-q", "-t", "ed25519", "-N", "", "-f", str(key)])
            self.assertEqual(result.returncode, 0, result.stderr)
        self.signers = self.tmp / "allowed_signers"
        self.write_signers(self.signers)
        self.addCleanup(self.assert_no_real_gh)

    def assert_no_real_gh(self):
        self.assertFalse(self.gh_attempts.exists(),
                         "SUT bypassed the stub transport and attempted gh")

    def proc(self, argv, input_text=None):
        return REAL_RUN([str(arg) for arg in argv], input=input_text, text=True,
                        capture_output=True, cwd=ROOT, env=self.env, timeout=25)

    def gate(self, *args):
        return self.proc([sys.executable, GATE, *args])

    def write_signers(self, path, principal=PRINCIPAL, namespace=NAMESPACE):
        public = " ".join(Path(str(self.key) + ".pub").read_text().split()[:2])
        path.write_text(f'{principal} namespaces="{namespace}" {public}\n', encoding="utf-8")

    def signature(self, *, repo=REPO, pr=PR, head=HEAD, key=None, namespace=NAMESPACE):
        result = self.proc([self.ssh, "-Y", "sign", "-f", key or self.key,
                            "-n", namespace], payload(repo, pr, head))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("-----BEGIN SSH SIGNATURE-----", result.stdout)
        return result.stdout

    def verify(self, comments, *, signers=None, raw=False, repo=REPO, pr=PR, head=HEAD):
        path = self.tmp / "comments.json"
        path.write_text(comments if raw else json.dumps(comments), encoding="utf-8")
        return self.gate("verify", "--repo", repo, "--pr", str(pr), "--head", head,
                         "--signers", signers or self.signers, "--comments", path)

    def accepted(self, result):
        self.assertEqual(result.returncode, 0, (result.stdout, result.stderr))
        self.assertRegex(result.stdout, r"(?m)^delivery-gate: accepted\b")
        self.assertNotIn("delivery-gate: refused:", result.stdout + result.stderr)

    def refused(self, result):
        self.assertEqual(result.returncode, 1, (result.stdout, result.stderr))
        self.assertNotIn("delivery-gate: accepted", result.stdout + result.stderr)
        reasons = result.stderr.splitlines()
        self.assertTrue(reasons, "refusal must explain itself on stderr")
        for reason in reasons:
            self.assertRegex(reason, r"^delivery-gate: refused: \S")


class SignatureRefusal(Sandbox):
    def test_positive_independent_signature_and_real_sign_command(self):
        # An all-refusing implementation cannot satisfy this control. Also
        # catches a sign/verify pair sharing the same WRONG payload/namespace.
        self.accepted(self.verify([{"body": comment(self.signature())}]))
        signed = self.gate("sign", "--repo", REPO, "--pr", str(PR), "--head", HEAD,
                           "--key", self.key)
        self.assertEqual(signed.returncode, 0, signed.stderr)
        self.assertEqual(signed.stdout.splitlines()[0], MARKER)
        armor = re.search(r"-----BEGIN SSH SIGNATURE-----.*?-----END SSH SIGNATURE-----",
                          signed.stdout, re.S)
        self.assertIsNotNone(armor, "sign must print an armored SSH signature")
        signature_file = self.tmp / "generated.sig"
        signature_file.write_text(armor.group(0) + "\n", encoding="utf-8")
        independent = self.proc([self.ssh, "-Y", "verify", "-f", self.signers,
                                 "-I", PRINCIPAL, "-n", NAMESPACE,
                                 "-s", signature_file], payload())
        self.assertEqual(independent.returncode, 0, independent.stderr)
        self.accepted(self.verify([{"id": 123, "body": signed.stdout}]))

    def test_no_comment_or_no_marker_cannot_authorize_direct_enqueue(self):
        good_armor_without_marker = comment(self.signature()).replace(MARKER, "", 1)
        for entries in ([], [{"body": "ordinary discussion"}],
                        [{"body": good_armor_without_marker}]):
            with self.subTest(comments=entries):
                self.refused(self.verify(entries))

    def test_forged_plain_text_garbage_or_truncated_signature_is_refused(self):
        for body in (
            MARKER + "\ndelivery gate passed\n",
            MARKER + "\n-----BEGIN SSH SIGNATURE-----\nnot-base64\n-----END SSH SIGNATURE-----\n",
            MARKER + "\n-----BEGIN SSH SIGNATURE-----\ntruncated\n",
        ):
            with self.subTest(body=body):
                self.refused(self.verify([{"body": body}]))

    def test_valid_signature_cannot_be_replayed_for_another_binding(self):
        for changes in ({"pr": PR + 1}, {"head": "b" * 40}, {"repo": "other/cadence"}):
            with self.subTest(changes=changes):
                binding = {"repo": REPO, "pr": PR, "head": HEAD, **changes}
                # Human text agrees with the replayed signature. A verifier
                # trusting that text instead of its CLI binding would accept.
                body = comment(self.signature(**binding), **binding)
                self.refused(self.verify([{"body": body}]))

    def test_comment_text_is_not_the_payload(self):
        body = MARKER + "\nUntrusted prose: repo attacker/project, pr 999, head deadbeef.\n"
        self.accepted(self.verify([{"body": body + self.signature()}]))

    def test_unlisted_key_cannot_authorize(self):
        self.refused(self.verify([{"body": comment(self.signature(key=self.other_key))}]))

    def test_wrong_allowed_signers_namespace_or_principal_is_refused(self):
        body = comment(self.signature())
        for principal, namespace in (("someone-else", NAMESPACE), (PRINCIPAL, "other-namespace")):
            with self.subTest(principal=principal, namespace=namespace):
                path = self.tmp / "wrong-signers"
                self.write_signers(path, principal, namespace)
                self.refused(self.verify([{"body": body}], signers=path))
        self.refused(self.verify([{"body": comment(self.signature(namespace="other-namespace"))}]))

    def test_empty_or_missing_signers_fails_closed(self):
        empty = self.tmp / "empty-signers"
        empty.write_text("", encoding="utf-8")
        body = comment(self.signature())
        for path in (empty, self.tmp / "nonexistent-signers"):
            with self.subTest(signers=path.name):
                self.refused(self.verify([{"body": body}], signers=path))

    def test_malformed_comments_fail_closed_even_beside_a_valid_signature(self):
        good = {"body": comment(self.signature())}
        for raw in ("{not json", "null", "{}", json.dumps(good), '"discussion"',
                    '[{}]', '["discussion"]', '[{"body":null}]', '[{"body":42}]',
                    json.dumps([good, {}])):
            with self.subTest(raw=raw):
                self.refused(self.verify(raw, raw=True))
        self.refused(self.gate("verify", "--repo", REPO, "--pr", str(PR), "--head", HEAD,
                               "--signers", self.signers,
                               "--comments", self.tmp / "missing-comments.json"))

    def test_valid_block_is_found_after_bad_blocks_and_comments(self):
        bad = "-----BEGIN SSH SIGNATURE-----\ngarbage\n-----END SSH SIGNATURE-----\n"
        good = self.signature()
        self.accepted(self.verify([{"body": MARKER + "\n" + bad},
                                   {"body": comment(good)}]))
        self.accepted(self.verify([{"body": MARKER + "\n" + bad + "\n" + good}]))

    def test_bad_cli_binding_is_usage_error_not_an_acceptance_or_refusal(self):
        for changes in ({"pr": "0"}, {"head": "a" * 39}, {"repo": "not-a-repo"}):
            binding = {"repo": REPO, "pr": str(PR), "head": HEAD, **changes}
            for subcommand in ("sign", "verify"):
                with self.subTest(binding=binding, subcommand=subcommand):
                    args = [subcommand, "--repo", binding["repo"], "--pr", binding["pr"],
                            "--head", binding["head"]]
                    if subcommand == "sign":
                        args += ["--key", self.key]
                    else:
                        comments = self.tmp / "usage-comments.json"
                        comments.write_text("[]", encoding="utf-8")
                        args += ["--signers", self.signers, "--comments", comments]
                    result = self.gate(*args)
                    self.assertEqual(result.returncode, 2, (result.stdout, result.stderr))
                    self.assertNotIn("delivery-gate: accepted", result.stdout)

    def test_queue_ref_is_exact_and_main_only(self):
        good = f"gh-readonly-queue/main/pr-{PR}-{HEAD}"
        for ref in (good, "refs/heads/" + good):
            with self.subTest(valid=ref):
                result = self.gate("queue-pr", ref)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), str(PR))
        for ref in (good.replace("/main/", "/staging/"), good[:-1],
                    f"gh-readonly-queue/main/pr-0-{HEAD}", good + "-extra",
                    good + "/extra", good + "\n", "prefix/" + good,
                    good.replace(HEAD, "g" * 40), "gh-readonly-queue/main/pr-73",
                    "refs/tags/" + good):
            with self.subTest(invalid=ref):
                result = self.gate("queue-pr", ref)
                self.assertEqual(result.returncode, 1, (result.stdout, result.stderr))
                self.assertEqual(result.stdout.strip(), "")


class EnqueueRefusal(Sandbox):
    def exercise(self, *, dry_run=False, missing_mode="default", valid_key=False,
                 post_failure=False):
        # Use the existing external-transport fixture, NOT a substitute main,
        # evaluate, signature guard or enqueue implementation.
        env = dict(self.env, ENQ_SCRIPT=str(ENQUEUE))
        missing = self.tmp / "missing-signing-key"
        if missing_mode == "environment":
            env["CADENCE_DELIVERY_GATE_KEY"] = str(missing)
        if valid_key:
            env["CADENCE_DELIVERY_GATE_KEY"] = str(self.key)
        extra = ["--gate-key", str(missing)] if missing_mode == "flag" else []
        gate_calls, gh_posts, order = [], [], []
        with mock.patch.dict(os.environ, env, clear=True), mock.patch.object(
            sys, "dont_write_bytecode", True), mock.patch.object(
            tempfile, "tempdir", self.env["TMPDIR"]):
            loader = importlib.machinery.SourceFileLoader(
                "cad1925_external_transports", str(ROOT / "scripts/test_enqueue_evidence.py"))
            spec = importlib.util.spec_from_loader(loader.name, loader)
            fixture = importlib.util.module_from_spec(spec)
            loader.exec_module(fixture)
            enq, case = fixture.enq, fixture.Case(self.tmp)
            self.assertEqual(Path(enq.__file__).resolve(), ENQUEUE,
                             "fixture must load the real enqueue-reviewed")

            def transport(argv, input_text=None):
                words = [str(arg) for arg in argv]
                if any(Path(word).resolve() == GATE for word in words[:2]):
                    self.assertIn("sign", words, "only the real signing command is expected here")
                    self.assertGreaterEqual(case.verdict_calls - initial_verdict_reads, 2,
                        "signing must follow preflight AND final gate re-evaluation")
                    gate_calls.append(words)
                    order.append("sign")
                    result = self.proc(words, input_text)
                    return result.returncode, result.stdout, result.stderr
                if words[:3] == ["gh", "pr", "comment"]:
                    gh_posts.append(input_text)
                    order.append("post")
                    return (1, "", "synthetic comment write refused") if post_failure else (0, "", "")
                if words[:3] == ["gh", "pr", "merge"]:
                    order.append("merge")
                return case.run(words, input_text)

            def deny_unexpected_process(argv, *args, **kwargs):
                raise AssertionError(f"enqueue bypassed the stubbed run transport: {argv!r}")

            with mock.patch.object(enq, "run", transport), mock.patch.object(
                enq, "pr_changes", lambda *a: (
                    [("M", "100644", "100644", p) for p in case.paths], 40)), mock.patch(
                "subprocess.run", side_effect=deny_unexpected_process):
                args = types.SimpleNamespace(pr=5, head=fixture.HEAD, repo=fixture.REPO,
                    notes_dir=str(self.tmp), author=[], dry_run=False)
                evidence = {}
                reasons, _ = enq.evaluate(args, fixture.REPO, require_comment=False, evidence=evidence)
                self.assertEqual(reasons, [], "all OTHER gates must actually pass the real evaluator")
                case.comments = [{"body": enq.evidence_body(evidence)}]
                reasons, _ = enq.evaluate(args, fixture.REPO)
                self.assertEqual(reasons, [], "the existing evidence-comment gate must also pass")
                argv = ["5", "--head", fixture.HEAD, "--repo", fixture.REPO,
                        "--notes-dir", str(self.tmp), "--poll-secs", "0", *extra]
                if dry_run:
                    argv.append("--dry-run")
                initial_verdict_reads = case.verdict_calls
                out, err = io.StringIO(), io.StringIO()
                with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                    rc = enq.main(argv)
        return rc, out.getvalue(), err.getvalue(), case, gate_calls, gh_posts, order

    def test_missing_key_never_invokes_gh_pr_merge(self):
        for mode in ("default", "environment", "flag"):
            with self.subTest(key_source=mode):
                rc, out, err, case, _, posts, _ = self.exercise(missing_mode=mode)
                self.assertEqual(rc, 1, (out, err))
                self.assertRegex(err, r"(?i)delivery[- ]gate|signing|signature")
                self.assertEqual(case.merges, [], "missing key reached gh pr merge")
                self.assertEqual(posts, [], "failed signing must not post a PR comment")

    def test_dry_run_needs_no_key_and_never_signs_posts_or_merges(self):
        rc, out, err, case, sign_calls, posts, order = self.exercise(dry_run=True)
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(sign_calls, [])
        self.assertEqual(posts, [])
        self.assertEqual(case.posted, [])
        self.assertEqual(case.merges, [])
        self.assertEqual(order, [])

    def test_normal_flow_posts_a_real_bound_signature_before_fake_enqueue(self):
        rc, out, err, case, sign_calls, posts, order = self.exercise(valid_key=True)
        self.assertEqual(rc, 0, (out, err))
        self.assertEqual(len(sign_calls), 1)
        self.assertEqual(len(posts), 1)
        self.assertEqual(len(case.merges), 1)
        self.assertEqual(order, ["sign", "post", "merge"])
        self.accepted(self.verify([{"body": posts[0]}], repo="o/r", pr=5, head="a" * 40))

    def test_failed_signature_comment_post_never_invokes_gh_pr_merge(self):
        rc, out, err, case, sign_calls, posts, order = self.exercise(
            valid_key=True, post_failure=True)
        self.assertEqual(rc, 1, (out, err))
        self.assertEqual(len(sign_calls), 1)
        self.assertEqual(len(posts), 1)
        self.assertEqual(order, ["sign", "post"])
        self.assertEqual(case.merges, [], "failed signature publication reached gh pr merge")


class WorkflowBaseTrust(unittest.TestCase):
    def test_merge_group_checks_out_base_and_uses_its_verifier_and_signers(self):
        # Narrow static contract check; later code review must inspect the real
        # workflow as well. In particular this does not solve workflow self-edit.
        text = (ROOT / ".github/workflows/delivery-gate.yml").read_text(encoding="utf-8")
        lines = text.splitlines()
        start = next((i for i, line in enumerate(lines)
                      if re.fullmatch(r" +delivery-gate:\s*(?:#.*)?", line)), None)
        self.assertIsNotNone(start, "required job id must be delivery-gate")
        indent = len(lines[start]) - len(lines[start].lstrip())
        end = next((i for i in range(start + 1, len(lines))
                    if lines[i].strip() and not lines[i].lstrip().startswith("#")
                    and len(lines[i]) - len(lines[i].lstrip()) <= indent), len(lines))
        job_lines = lines[start + 1:end]
        job = "\n".join(line for line in job_lines if not line.lstrip().startswith("#"))
        self.assertIn("merge_group", text)
        self.assertIn("pull_request", text)
        steps = [(i, len(line) - len(line.lstrip())) for i, line in enumerate(job_lines)
                 if re.match(r"\s*-\s+\w", line)]
        self.assertTrue(steps, "delivery-gate must have executable steps")
        step_indent = min(n for _, n in steps)
        starts = [i for i, n in steps if n == step_indent]
        blocks = ["\n".join(job_lines[a:b]) for a, b in
                  zip(starts, starts[1:] + [len(job_lines)])]
        checkouts = [block for block in blocks if re.search(
            r"(?m)^\s*(?:-\s*)?uses:\s*['\"]?actions/checkout@", block)]
        self.assertTrue(checkouts, "merge-group code must be checked out, not taken from a PR head")
        for block in checkouts:
            refs = re.findall(r"(?m)^\s+ref:\s*(.*?)\s*$", block)
            self.assertEqual(len(refs), 1, "checkout must explicitly pin the trusted base ref")
            ref = re.sub(r"\s+", "", refs[0].split(" #", 1)[0].strip().strip("'\""))
            self.assertEqual(ref, "${{github.event.merge_group.base_sha}}",
                             "PR/head/default checkout cannot supply the gate or trust root")
        run_blocks = [block for block in blocks if re.search(r"(?m)^\s*(?:-\s*)?run:", block)]
        code = "\n".join(line for block in run_blocks for line in block.splitlines()
                         if not line.lstrip().startswith("#"))
        code = code.replace("\\\n", "")
        self.assertRegex(code, r"scripts/delivery-gate['\"]?\s+queue-pr\b")
        self.assertRegex(code, r"scripts/delivery-gate['\"]?\s+verify\b")
        self.assertIn("merge_group.head_ref", job)
        self.assertRegex(code, r"--signers(?:\s+|=)['\"]?(?:\./)?docs/roles/delivery-gate\.allowed_signers\b")


if __name__ == "__main__":
    unittest.main()
