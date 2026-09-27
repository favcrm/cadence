"""Proposed model examples only, not production authorization or signature checks.

Run: python3 docs/design/project-releases-v1/validate.py
Requires the validation tool jsonschema; no Rust build or runtime writes.
"""

import copy
import hashlib
import json
import re
import unicodedata
import unittest
from datetime import datetime, timezone
from pathlib import Path

from jsonschema import Draft202012Validator, FormatChecker, ValidationError

ROOT = Path(__file__).parent
SCHEMA = json.loads((ROOT / "schema.json").read_text())
FIXTURE = json.loads((ROOT / "fixture.json").read_text())
VALIDATOR = Draft202012Validator(SCHEMA, format_checker=FormatChecker())
SEMVER = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def utc_time(value):
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    require(parsed.tzinfo is not None, "Observation time requires a timezone")
    return parsed.astimezone(timezone.utc)


def scope(context):
    require(
        (context["mode"] == "local" and context["organization_id"] is None)
        or (context["mode"] == "remote" and context["organization_id"] is not None),
        "Local context cannot fabricate a remote organization",
    )
    return tuple(context[key] for key in ("authority_id", "mode", "organization_id", "project", "stream"))


def version(value, scheme):
    require(len(value) <= 128 and value == value.strip(), "Version is bounded and trimmed")
    if scheme == "named":
        require(value and not any(ord(char) < 32 for char in value), "Invalid named release")
        require(unicodedata.normalize("NFC", value) == value, "Named release must use NFC")
        return value, None
    if scheme == "calendar":
        match = re.fullmatch(r"([0-9]{4})\.(0[1-9]|1[0-2])\.(0|[1-9][0-9]*)", value)
        require(match and int(match[1]) > 0, "Expected YYYY.MM.PATCH")
        return value, tuple(int(part) for part in match.groups())
    match = SEMVER.fullmatch(value)
    require(match is not None, "Invalid SemVer")
    identifiers = (match[4] or "").split(".") if match[4] else []
    require(
        all(not part.isdigit() or part == "0" or not part.startswith("0") for part in identifiers),
        "Leading zero in numeric prerelease",
    )
    precedence = tuple(int(match[n]) for n in (1, 2, 3)) + (
        0 if identifiers else 1,
        tuple((0, int(part)) if part.isdigit() else (1, part) for part in identifiers),
    )
    return value.split("+", 1)[0], precedence


def records(data, table):
    result = {}
    for item in data[table]:
        key = scope(item["context"]), item["id"]
        require(key not in result, "Duplicate scoped record ID")
        result[key] = item
    return result


def lookup(data, table, context, record_id):
    record = records(data, table).get((scope(context), record_id))
    require(record is not None, "Missing or cross-context reference")
    return record


def candidate_digest(data, candidate):
    payload = dict(candidate)
    payload.pop("snapshot_sha256", None)
    builds = sorted(
        [lookup(data, "builds", candidate["context"], key) for key in candidate["build_ids"]],
        key=lambda build: build["id"],
    )
    return hashlib.sha256(canonical({"candidate": payload, "builds": builds}).encode()).hexdigest()


def validate(data):
    VALIDATOR.validate(data)
    for table in ("policies", "releases", "builds", "candidates", "publications", "deployments", "audit_events"):
        records(data, table)
    keys = set()
    for release in data["releases"]:
        policy = lookup(data, "policies", release["context"], release["policy_id"])
        key, _ = version(release["version"], policy["scheme"])
        require(key == release["version_key"], "Version key differs from policy")
        identity = scope(release["context"]), key
        require(identity not in keys, "Release version collision")
        keys.add(identity)
        require((release["state"] == "planned") == (release["candidate_id"] is None), "Candidate/state mismatch")
        require((release["state"] == "published") == (release["publication_id"] is not None), "Publication/state mismatch")
        if release["candidate_id"]:
            candidate = lookup(data, "candidates", release["context"], release["candidate_id"])
            require(candidate["release_id"] == release["id"], "Candidate references another release")
        if release["publication_id"]:
            publication = lookup(data, "publications", release["context"], release["publication_id"])
            require(publication["release_id"] == release["id"], "Publication references another release")
    targets = set()
    links = set()
    for link in data["issue_links"]:
        lookup(data, "releases", link["context"], link["release_id"])
        key = scope(link["context"]), link["issue_id"], link["kind"], link["release_id"]
        require(key not in links, "Duplicate issue relationship")
        links.add(key)
        if link["kind"] == "target":
            key = scope(link["context"]), link["issue_id"]
            require(key not in targets, "Issue has more than one target release")
            targets.add(key)
        else:
            require(link["issue_type"] == "bug", "Affected membership is for bugs")
    for candidate in data["candidates"]:
        release = lookup(data, "releases", candidate["context"], candidate["release_id"])
        require(candidate["policy_id"] == release["policy_id"] and candidate["version"] == release["version"], "Candidate version drift")
        require(set(candidate["proposed_shipped_issues"]) <= set(candidate["target_scope_issues"]), "Unplanned candidate membership")
        require(candidate_digest(data, candidate) == candidate["snapshot_sha256"], "Candidate source/scope/artifact drift")
    imports = set()
    for publication in data["publications"]:
        candidate = lookup(data, "candidates", publication["context"], publication["candidate_id"])
        release = lookup(data, "releases", publication["context"], publication["release_id"])
        require(candidate["release_id"] == release["id"], "Publication candidate mismatch")
        for field in ("policy_id", "version", "version_key"):
            require(publication[field] == release[field], "Published version provenance mismatch")
        require(publication["build_ids"] == candidate["build_ids"] and publication["shipped_issues"] == candidate["proposed_shipped_issues"], "Published candidate membership mismatch")
        require(publication["candidate_sha256"] == candidate["snapshot_sha256"], "Published source snapshot mismatch")
        require(release["publication_id"] == publication["id"] and release["candidate_id"] == candidate["id"], "Publication is not the release receipt")
        evidence = publication["evidence"]
        if evidence["kind"] == "github":
            key = scope(publication["context"]), evidence["repository"], evidence["upstream_release_id"]
            require(key not in imports, "Duplicate upstream receipt")
            imports.add(key)
            builds = [lookup(data, "builds", publication["context"], key) for key in publication["build_ids"]]
            sources = [source for build in builds for source in build["sources"]]
            require(any(source["kind"] == "git" and source["repository"] == evidence["repository"] and source["commit"] == evidence["commit"] for source in sources), "GitHub source mismatch")
            expected_assets = sorted((artifact["name"], artifact["sha256"], artifact["size"]) for build in builds for artifact in build["artifacts"])
            actual_assets = sorted((asset["name"], asset["sha256"], asset["size"]) for asset in evidence["assets"])
            require(expected_assets == actual_assets, "GitHub artifact mismatch")
        else:
            key = scope(publication["context"]), "manual", evidence["attestation_id"]
            require(key not in imports, "Duplicate manual attestation")
            imports.add(key)
            require(evidence["attester"] == publication["publisher"], "Manual attester differs from publisher")
    for deployment in data["deployments"]:
        release = lookup(data, "releases", deployment["context"], deployment["release_id"])
        require(release["state"] == "published", "Deployment cannot infer publication")
        publication = lookup(data, "publications", deployment["context"], release["publication_id"])
        require(deployment["build_id"] in publication["build_ids"], "Deployment build does not match release")
        for relation in ("previous_id", "rollback_to_id"):
            if deployment[relation]:
                previous = lookup(data, "deployments", deployment["context"], deployment[relation])
                require(previous["environment"] == deployment["environment"] and utc_time(previous["observed_at"]) < utc_time(deployment["observed_at"]), "Deployment history mismatch")
                if relation == "rollback_to_id":
                    require((previous["release_id"], previous["build_id"]) == (deployment["release_id"], deployment["build_id"]), "Rollback changed historical contents")
    return data


def validate_transition(before, after):
    """Example immutability constraint only; no actor/custody verification."""
    validate(before)
    validate(after)
    for table in ("policies", "builds", "candidates", "publications", "deployments", "audit_events"):
        updated = records(after, table)
        for key, old in records(before, table).items():
            require(updated.get(key) == old, "Immutable history was removed or rewritten")
    updated = records(after, "releases")
    for key, old in records(before, "releases").items():
        new = updated.get(key)
        require(new is not None, "Release cannot be deleted")
        for field in ("policy_id", "version", "version_key"):
            require(new[field] == old[field], "Release identity cannot be rewritten")
        if old["state"] == "published":
            for field in ("state", "candidate_id", "publication_id"):
                require(new[field] == old[field], "Published provenance cannot be rewritten")
        if new != old:
            require(new["revision"] == old["revision"] + 1, "Changed release requires next revision")
            require(any(event["context"] == new["context"] and event["record_id"] == new["id"] and event["before_revision"] == old["revision"] and event["after_revision"] == new["revision"] for event in after["audit_events"] if (scope(event["context"]), event["id"]) not in records(before, "audit_events")), "Changed release requires appended audit")


def import_replay(existing, incoming):
    """A deterministic decision example, not an external importer."""
    require(existing["context"] == incoming["context"], "Cross-context import")
    old, new = existing["evidence"], incoming["evidence"]
    require(old["kind"] == new["kind"] == "github", "Expected GitHub evidence")
    require((old["repository"], old["upstream_release_id"]) == (new["repository"], new["upstream_release_id"]), "Different external identity")
    old_snapshot = {key: value for key, value in existing.items() if key != "id"}
    new_snapshot = {key: value for key, value in incoming.items() if key != "id"}
    require(canonical(old_snapshot) == canonical(new_snapshot), "Upstream receipt changed: conflict")
    return "unchanged"


class ModelExamples(unittest.TestCase):
    def test_schema_and_fixture(self):
        Draft202012Validator.check_schema(SCHEMA)
        validate(FIXTURE)
        validate_transition(FIXTURE, copy.deepcopy(FIXTURE))

    def test_partial_delivery_backport_and_rollback(self):
        main = FIXTURE["candidates"][1]
        self.assertEqual(set(main["target_scope_issues"]) - set(main["proposed_shipped_issues"]), {"EX-3"})
        self.assertEqual(sum("EX-1" in receipt["shipped_issues"] for receipt in FIXTURE["publications"]), 2)
        self.assertEqual(FIXTURE["deployments"][2]["rollback_to_id"], "deploy-first")

    def test_same_version_in_another_project_stream_or_org(self):
        for context_patch in ({"project": "other"}, {"stream": "maintenance"},
                              {"mode": "remote", "organization_id": "org_other"},
                              {"authority_id": "another-local-tracker"}):
            after = copy.deepcopy(FIXTURE)
            release = copy.deepcopy(after["releases"][2])
            release["context"].update(context_patch)
            policy = copy.deepcopy(after["policies"][0])
            policy["context"] = release["context"]
            after["policies"].append(policy)
            after["releases"].append(release)
            validate(after)

    def test_concurrent_mainline_and_maintenance_targets(self):
        after = copy.deepcopy(FIXTURE)
        policy = copy.deepcopy(after["policies"][0])
        policy["context"]["stream"] = "maintenance"
        release = copy.deepcopy(after["releases"][2])
        release["context"] = policy["context"]
        after["policies"].append(policy)
        after["releases"].append(release)
        after["issue_links"].append({"context": policy["context"], "issue_id": "EX-1", "issue_type": "bug", "release_id": release["id"], "kind": "target"})
        validate(after)

    def test_policy_revision_keeps_history_and_collisions(self):
        after = copy.deepcopy(FIXTURE)
        policy = copy.deepcopy(after["policies"][0])
        policy.update(id="policy-new", scheme="named")
        after["policies"].append(policy)
        release = copy.deepcopy(after["releases"][2])
        release.update(id="rel-new", policy_id=policy["id"], version="Autumn Review", version_key="Autumn Review")
        after["releases"].append(release)
        validate_transition(FIXTURE, after)
        release.update(version="0.1.1", version_key="0.1.1")
        with self.assertRaises(ValueError):
            validate(after)

    def test_observation_times_compare_actual_instants(self):
        after = copy.deepcopy(FIXTURE)
        # Lexically later, but actually BEFORE 01:00Z.
        after["deployments"][1]["observed_at"] = "2026-09-27T02:00:00+02:00"
        with self.assertRaises(ValueError):
            validate(after)
        # Lexically earlier, but actually AFTER 01:00Z and BEFORE 03:00Z.
        after["deployments"][1]["observed_at"] = "2026-09-27T00:30:00-02:00"
        validate(after)

    def test_semver_precedence(self):
        values = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0"]
        keys = [version(value, "semver")[1] for value in values]
        self.assertEqual(keys, sorted(keys))
        self.assertEqual(version("1.0.0+one", "semver"), version("1.0.0+two", "semver"))

    def test_version_policies(self):
        for value in ("v1.0.0", "01.0.0", "1.0.0-01", "1.0.0-", "1.0.0+"):
            with self.assertRaises(ValueError):
                version(value, "semver")
        for value in ("2026.13.0", "2026.9.0", "0000.01.0", "2026.09.01"):
            with self.assertRaises(ValueError):
                version(value, "calendar")
        self.assertLess(version("2026.09.2", "calendar")[1], version("2026.10.0", "calendar")[1])
        self.assertEqual(version("Autumn Review", "named"), ("Autumn Review", None))

    def test_import_replay(self):
        publication = FIXTURE["publications"][0]
        self.assertEqual(import_replay(publication, copy.deepcopy(publication)), "unchanged")
        replay = copy.deepcopy(publication)
        replay["id"] = "newly-allocated-local-id"
        self.assertEqual(import_replay(publication, replay), "unchanged")
        changed = copy.deepcopy(publication)
        changed["evidence"]["commit"] = "c" * 40
        with self.assertRaises(ValueError):
            import_replay(publication, changed)

    def test_coherent_provenance_rewrite_still_changes_immutable_history(self):
        after = copy.deepcopy(FIXTURE)
        after["builds"][0]["sources"][0]["commit"] = "c" * 40
        candidate = after["candidates"][0]
        candidate["snapshot_sha256"] = candidate_digest(after, candidate)
        receipt = after["publications"][0]
        receipt["candidate_sha256"] = candidate["snapshot_sha256"]
        receipt["evidence"]["commit"] = "c" * 40
        validate(after)
        with self.assertRaises(ValueError):
            validate_transition(FIXTURE, after)

    def test_published_editorial_correction_requires_audit(self):
        after = copy.deepcopy(FIXTURE)
        release = after["releases"][0]
        release["notes"] = "Audited editorial correction; publication snapshot unchanged"
        release["revision"] += 1
        with self.assertRaises(ValueError):
            validate_transition(FIXTURE, after)
        event = copy.deepcopy(after["audit_events"][0])
        event.update(id="audit-editorial", record_id=release["id"], action="release.editorial_corrected", before_revision=1, after_revision=2, idempotency_key="operation-editorial")
        after["audit_events"].append(event)
        validate_transition(FIXTURE, after)


def mutation_test(change):
    def test(self):
        after = copy.deepcopy(FIXTURE)
        change(after)
        with self.assertRaises((ValueError, ValidationError)):
            validate(after)
    return test


MUTATIONS = {
    "cross_org_reference": lambda data: data["releases"][3]["context"].update(organization_id="org_other"),
    "fake_local_org": lambda data: data["releases"][0]["context"].update(organization_id="org_fake"),
    "wrong_project_reference": lambda data: data["issue_links"][0]["context"].update(project="other"),
    "duplicate_version": lambda data: data["releases"].append({**data["releases"][2], "id": "rel-duplicate"}),
    "archived_collision": lambda data: data["releases"].append({**data["releases"][2], "id": "rel-archive", "archived": True}),
    "multiple_target": lambda data: data["issue_links"].append({**data["issue_links"][0], "release_id": "rel-next"}),
    "affected_task": lambda data: data["issue_links"][3].update(issue_type="task"),
    "source_drift": lambda data: data["builds"][0]["sources"][0].update(commit="c" * 40),
    "artifact_drift": lambda data: data["builds"][0]["artifacts"][0].update(sha256="5" * 64),
    "scope_drift": lambda data: data["candidates"][0]["proposed_shipped_issues"].append("EX-4"),
    "receipt_member_rewrite": lambda data: data["publications"][0]["shipped_issues"].append("EX-3"),
    "publication_not_inferred": lambda data: data["releases"][2].update(state="published"),
    "github_asset_mismatch": lambda data: data["publications"][0]["evidence"]["assets"][0].update(sha256="6" * 64),
    "manual_attester_mismatch": lambda data: data["publications"][2]["evidence"].update(attester="other"),
    "deployment_build_mismatch": lambda data: data["deployments"][0].update(build_id="build-mainline"),
    "rollback_target_mismatch": lambda data: data["deployments"][2].update(rollback_to_id="deploy-second"),
    "rollback_environment_mismatch": lambda data: data["deployments"][2].update(environment="production"),
    "forged_extra_role": lambda data: data["publications"][0].update(role="operator"),
    "invalid_date": lambda data: data["releases"][2].update(target_date="2026-02-30"),
    "empty_stream": lambda data: data["releases"][2]["context"].update(stream=""),
    "unsupported_contract": lambda data: data.update(contract="cadence.project-releases.v999"),
}
for name, mutation in MUTATIONS.items():
    setattr(ModelExamples, "test_refuse_" + name, mutation_test(mutation))


if __name__ == "__main__":
    unittest.main()
