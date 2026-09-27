#!/usr/bin/env python3
"""Adversarial contracts: cancellation must require every safety proof."""
import copy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch
from urllib.error import HTTPError

SCRIPT = Path(__file__).resolve().parents[2] / "scripts/ci-obsolete-merge-groups.py"
SPEC = importlib.util.spec_from_file_location("obsolete", SCRIPT)
obsolete = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(obsolete)
SHA = "a" * 40
MAIN = "b" * 40
REF = "gh-readonly-queue/main/pr-660-" + "c" * 40
REPOSITORY = {"id": 1372936414, "full_name": "favcrm/cadence"}


def run():
    return {"id": 42, "event": "merge_group", "head_branch": REF,
            "head_sha": SHA, "path": ".github/workflows/ci.yml",
            "workflow_id": 359597313, "run_attempt": 1, "status": "in_progress",
            "repository": REPOSITORY, "head_repository": REPOSITORY}


class FakeAPI:
    def __init__(self):
        self.runs = [run()]
        self.detail = run()
        self.ref = None
        self.main = MAIN
        self.queue = set()
        self.compare = "diverged"
        self.cancelled = []
        self.fail = None
        self.before_check = None
        self.checks = 0

    def repository(self):
        return REPOSITORY

    def runs_for_branch(self, branch):
        if self.fail:
            raise obsolete.Unsafe(self.fail)
        return copy.deepcopy(self.runs)

    def current_run(self, run_id):
        self.checks += 1
        if self.before_check:
            self.before_check(self)
        return copy.deepcopy(self.detail)

    def branch_sha(self, branch):
        return self.main if branch == "main" else self.ref

    def queue_heads(self):
        return self.queue

    def comparison(self, sha, main):
        return {"status": self.compare, "base_commit": {"sha": sha}}

    def cancel(self, run_id):
        self.cancelled.append(run_id)


class CancellationContracts(unittest.TestCase):
    def sweep(self, api, **kwargs):
        event = {"ref_type": "branch", "ref": REF, "repository": REPOSITORY}
        obsolete.clean_deleted_group(api, event, apply=True, **kwargs)

    def test_deleted_stale_group_can_cancel(self):
        api = FakeAPI()
        self.sweep(api)
        self.assertEqual(api.cancelled, [42])
        self.assertEqual(api.checks, 2)

    def test_current_branch_or_queue_head_never_cancel(self):
        for field, value in (("ref", SHA), ("ref", MAIN), ("queue", {SHA})):
            with self.subTest(field=field, value=value):
                api = FakeAPI()
                setattr(api, field, value)
                self.sweep(api)
                self.assertEqual(api.cancelled, [])

    def test_merged_main_evidence_and_ambiguous_compare_are_preserved(self):
        for status in ("ahead", "identical", "unknown", None):
            with self.subTest(status=status):
                api = FakeAPI()
                api.compare = status
                self.sweep(api)
                self.assertEqual(api.cancelled, [])

    def test_only_exact_allowlisted_run_identity(self):
        cases = [("event", "push"), ("event", "pull_request"),
                 ("head_branch", "main"), ("head_branch", "v1.0"),
                 ("path", ".github/workflows/other.yml"), ("workflow_id", 7),
                 ("head_sha", "bad"), ("run_attempt", 0), ("status", "completed"),
                 ("repository", {"id": 7, "full_name": "favcrm/cadence"}),
                 ("head_repository", {"id": 7, "full_name": "evil/cadence"})]
        for field, value in cases:
            with self.subTest(field=field, value=value):
                api = FakeAPI()
                api.runs[0][field] = value
                api.detail[field] = value
                self.sweep(api)
                self.assertEqual(api.cancelled, [])

    def test_reused_name_different_head_is_ambiguous(self):
        api = FakeAPI()
        other = run()
        other.update(id=43, head_sha=MAIN, status="completed")
        api.runs.append(other)
        self.sweep(api)
        self.assertEqual(api.cancelled, [])

    def test_api_errors_permissions_and_pagination_fail_closed(self):
        for failure in ("HTTP403", "HTTP500", "incomplete pagination"):
            api = FakeAPI()
            api.fail = failure
            self.sweep(api)
            self.assertEqual(api.cancelled, [])

    def test_state_changes_before_post_are_rechecked(self):
        def update(api):
            if api.checks == 2:
                api.ref = SHA
        api = FakeAPI()
        api.before_check = update
        self.sweep(api)
        self.assertEqual(api.cancelled, [])
        for field, value in (("head_sha", MAIN), ("run_attempt", 2),
                             ("event", "push"), ("status", "completed")):
            api = FakeAPI()
            api.before_check = lambda a, f=field, v=value: a.detail.update({f: v}) if a.checks == 2 else None
            self.sweep(api)
            self.assertEqual(api.cancelled, [])

    def test_queue_and_main_changes_at_final_proof_retain(self):
        api = FakeAPI()
        api.before_check = lambda a: a.queue.add(SHA) if a.checks == 2 else None
        self.sweep(api)
        self.assertEqual(api.cancelled, [])
        api = FakeAPI()
        original = api.branch_sha
        count = 0
        def moving(branch):
            nonlocal count
            if branch == "main":
                count += 1
                if count == 4:
                    return SHA
            return original(branch)
        api.branch_sha = moving
        self.sweep(api)
        self.assertEqual(api.cancelled, [])

    def test_forged_delete_payload_and_dry_run_cannot_cancel(self):
        for field, value in (("ref_type", "tag"), ("ref", "main"),
                             ("repository", {"id": 7, "full_name": "favcrm/cadence"})):
            api = FakeAPI()
            event = {"ref_type": "branch", "ref": REF, "repository": REPOSITORY}
            event[field] = value
            obsolete.clean_deleted_group(api, event, apply=True)
            self.assertEqual(api.cancelled, [])
        api = FakeAPI()
        event = {"ref_type": "branch", "ref": REF, "repository": REPOSITORY}
        obsolete.clean_deleted_group(api, event, apply=False)
        self.assertEqual(api.cancelled, [])


class APIContracts(unittest.TestCase):
    def test_404_alone_and_other_http_errors_cannot_prove_absence(self):
        api = obsolete.GitHub("fake-token")
        for status in (403, 404, 500):
            error = HTTPError("https://api.github.com/test", status, "error", {}, None)
            with patch.object(api.opener, "open", side_effect=error):
                with self.assertRaises(obsolete.Unsafe):
                    api.branch_sha(REF)
                with self.assertRaises(obsolete.Unsafe):
                    api.branch_sha("main")
                with self.assertRaises(obsolete.Unsafe):
                    api.current_run(42)

    def test_absence_needs_successful_empty_matching_refs(self):
        api = obsolete.GitHub("fake-token")
        with patch.object(api, "request", side_effect=[None, []]):
            self.assertIsNone(api.branch_sha(REF))
        for value in (None, {}, [{"ref": "refs/heads/" + REF}]):
            with patch.object(api, "request", side_effect=[None, value]):
                with self.assertRaises(obsolete.Unsafe):
                    api.branch_sha(REF)

    def test_runs_follow_all_pages_and_refuse_incomplete_or_reused_pages(self):
        api = obsolete.GitHub("fake-token")
        first = [dict(run(), id=index) for index in range(1, 101)]
        pages = [{"total_count": 101, "workflow_runs": first},
                 {"total_count": 101, "workflow_runs": [dict(run(), id=101)]}]
        with patch.object(api, "request", side_effect=pages) as request:
            self.assertEqual(len(api.runs_for_branch(REF)), 101)
            self.assertIn("page=2", request.call_args[0][0])
        for second in ({"total_count": 101, "workflow_runs": []},
                       {"total_count": 101, "workflow_runs": [first[0]]},
                       {"total_count": 102, "workflow_runs": [dict(run(), id=101)]}):
            with patch.object(api, "request", side_effect=[pages[0], second]):
                with self.assertRaises(obsolete.Unsafe):
                    api.runs_for_branch(REF)

    def queue_page(self, nodes, more, cursor, total=101):
        return {"data": {"repository": {"databaseId": obsolete.REPO_ID,
                "mergeQueue": {"entries": {"totalCount": total, "nodes": nodes,
                "pageInfo": {"hasNextPage": more, "endCursor": cursor}}}}}}

    def test_queue_fully_paginated_and_active_head_on_last_page_kept(self):
        api = obsolete.GitHub("fake-token")
        nodes = [{"id": str(index), "headCommit": {"oid": f"{index:040x}"}} for index in range(100)]
        pages = [self.queue_page(nodes, True, "page-1"),
                 self.queue_page([{"id": "last", "headCommit": {"oid": SHA}}], False, "page-2")]
        with patch.object(api, "request", side_effect=pages) as request:
            self.assertIn(SHA, api.queue_heads())
            self.assertEqual(request.call_args.kwargs["body"]["variables"]["cursor"], "page-1")
        for page in (self.queue_page([], False, "page-2"),
                     self.queue_page([nodes[0]], False, "page-2"),
                     {"errors": [{"message": "no permission"}]}):
            with patch.object(api, "request", side_effect=[pages[0], page]):
                with self.assertRaises(obsolete.Unsafe):
                    api.queue_heads()

    def test_redirect_does_not_forward_credentials(self):
        with self.assertRaises(obsolete.Unsafe):
            obsolete.NoRedirect().redirect_request(None, None, 302, "redirect", {}, "https://evil.example")


if __name__ == "__main__":
    unittest.main()
