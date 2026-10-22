#!/usr/bin/env python3
"""CAD-1455 INDEPENDENT integration acceptance — immutable writer replay/conflict.

Authored independently of the implementer; the implementer may not edit
or weaken this file.

Check 3 of the CAD-1455 integration refusal set. It runs the REAL
`scripts/publish-release.py` end to end against an isolated local `aws`
shim that models ONLY object-storage conditional create / read / head on
an on-disk namespace — no credentials, no network, no real R2. The shim
is a transparent, logging stand-in: it implements exactly the three
s3api operations the publisher calls, honors `--if-none-match '*'` for
real (a second put on an existing key fails PreconditionFailed, the
same refusal R2 returns), and records every call to a log so the check
asserts on what the publisher actually asked storage to do, not on a
fabricated outcome.

What it proves (the ticket's publisher guards):

  POSITIVE — a first complete publish writes every artifact object
    conditionally, writes the catalogue record LAST, reads it back and
    HEADs each object. Exit 0, `published:true`, stored catalogue bytes
    match the settled AOS release-entry schema (id == product/profile/
    version, status published, strict fields, sha256:<64hex> digests
    matching the stored object bytes).

  IDENTICAL RETRY — the same publish re-run with IDENTICAL inputs must
    succeed idempotently and the stored catalogue record must keep the
    FIRST write's metadata (recordedAt above all) rather than stamping
    the retry's clock and conflicting on its own output. The check injects
    distinct clocks into the real publisher subprocess so a fresh per-invocation
    `recordedAt` is observably different without sleeping. RED at baseline:
    the script generates a fresh `recordedAt` every invocation and byte-compares
    the entry, so an identical replay across a changed clock reports
    `conflict` — the exact identical-retry violation the ticket names.

  CONFLICT — re-publishing the same id with CHANGED artifact bytes (or
    changed facts) refuses without overwriting the stored object or the
    catalogue record; both stay byte-identical to the first publish.

  UNSUPPORTED WITHDRAWAL — `--status withdrawn`/`--withdrawn-at` must
    never leave a published catalogue record while reporting withdrawn.
    At baseline the flags are accepted but `entry_document` hardcodes
    `status: published`/`withdrawnAt: null`, so the script can print
    `"status": "withdrawn"` while the stored record stays published.
    GREEN: the unsupported flags refuse outright (documented
    not-implemented) so no record is written and nothing claims success.

LIMITS: local deterministic S3 semantics only. Real R2 conditional
writes, partial-failure windows, cross-service and credential behavior
remain UNQUALIFIED — the shim is transparent plumbing, not authority.
"""
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(
    os.environ.get("CAD1455_REPO", Path(__file__).resolve().parents[1])
)
PUBLISHER = REPO / "scripts" / "publish-release.py"

SOURCE_SHA = "1" * 40
VERSION = SOURCE_SHA  # operator-host releases are keyed by source sha
ENDPOINT = "https://sham.local.r2.invalid"
BUCKET = "releases-test"

# Settled AOS contract field set for a release entry (the inspected
# aos255 snapshot at /tmp/.../aos255-releases-contract-3d084e5a.ts).
ENTRY_FIELDS = {
    "id", "product", "version", "sourceSha", "profile", "visibility",
    "status", "publishedAt", "withdrawnAt", "recordedAt",
    "productionCandidateId", "artifacts",
}
ISO = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$")
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


# The local `aws` shim. Implements only put-object (with real
# if-none-match conflict), get-object, head-object against an on-disk
# store dir; appends every invocation to $SHIM_LOG.
SHIM = r'''#!/usr/bin/env python3
import hashlib, json, os, sys
store = os.environ["SHIM_STORE"]; os.makedirs(store, exist_ok=True)
log = os.environ.get("SHIM_LOG")
def rec(op, key):
    if log:
        with open(log, "a") as f: f.write(op + " " + key + "\n")
def kf(key): return os.path.join(store, "obj__" + key.replace("/", "_"))
def mf(key): return kf(key) + ".meta"
args = sys.argv[1:]
if len(args) < 2 or args[0] != "s3api":
    print("shim: expected s3api", file=sys.stderr); sys.exit(2)
op = args[1]
def val(flag):
    i = args.index(flag); return args[i + 1]
key = val("--key") if "--key" in args else ""
rec(op, key)
if op == "put-object":
    body = val("--body")
    if "--if-none-match" in args and os.path.exists(kf(key)):
        print("An error occurred (PreconditionFailed) when calling PutObject: if-none-match",
              file=sys.stderr); sys.exit(1)
    data = open(body, "rb").read()
    open(kf(key), "wb").write(data)
    open(mf(key), "w").write(json.dumps({
        "ETag": '"%s"' % hashlib.sha256(data).hexdigest(),
        "ContentLength": len(data),
        "ContentType": val("--content-type") if "--content-type" in args else ""}))
    print(json.dumps({"ETag": '"%s"' % hashlib.sha256(data).hexdigest()}))
elif op == "get-object":
    dest = args[-1]
    if not os.path.exists(kf(key)):
        print("An error occurred (NoSuchKey) when calling GetObject", file=sys.stderr); sys.exit(1)
    open(dest, "wb").write(open(kf(key), "rb").read())
    print(json.dumps({"ContentLength": os.path.getsize(kf(key))}))
elif op == "head-object":
    if not os.path.exists(mf(key)):
        print("An error occurred (404) when calling HeadObject: Not Found", file=sys.stderr); sys.exit(1)
    print(open(mf(key)).read())
else:
    print("shim: unsupported op " + op, file=sys.stderr); sys.exit(2)
'''


class PublishCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="c1455-pub-")
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.tools = self.dir / "tools"
        self.tools.mkdir()
        aws = self.tools / "aws"
        aws.write_text(SHIM)
        aws.chmod(aws.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        self.store = self.dir / "bucket"
        self.store.mkdir()
        self.log = self.dir / "aws-calls.log"
        self.src = self.dir / "src"
        self.src.mkdir()
        self.artifact = self.src / "cadence-release.tar.gz"
        self.artifact.write_bytes(b"FIXTURE-TARBALL-BYTES-" + b"x" * 64)

    def env(self):
        e = dict(os.environ)
        e["PATH"] = f"{self.tools}:{e['PATH']}"
        e["SHIM_STORE"] = str(self.store)
        e["SHIM_LOG"] = str(self.log)
        # Dummy credential env satisfies the publisher's own
        # not_configured gate; the shim never reads them.
        e["AWS_ACCESS_KEY_ID"] = "sham-key-id"
        e["AWS_SECRET_ACCESS_KEY"] = "sham-secret"
        e.pop("AWS_PROFILE", None)
        return e

    def publish(self, *extra, artifacts=None, recorded_at=None):
        args = [sys.executable, str(PUBLISHER),
                "--endpoint", ENDPOINT, "--bucket", BUCKET,
                "--product", "cadence", "--profile", "operator-host",
                "--version", VERSION, "--source-sha", SOURCE_SHA,
                "--visibility", "private",
                "--production-candidate-id", "987654321"]
        specs = artifacts if artifacts is not None else [
            f"cadence-release.tar.gz={self.artifact}:x86_64-linux:application/octet-stream"]
        for spec in specs:
            args += ["--artifact", spec]
        args += list(extra)
        if recorded_at is not None:
            # Execute the real entrypoint, replacing only its clock in this
            # isolated subprocess. Storage, parsing and guards remain real.
            clock_runner = (
                "import runpy,sys; path=sys.argv.pop(1); stamp=sys.argv.pop(1); "
                "module=runpy.run_path(path); "
                "module['main'].__globals__['_utcnow']=lambda: stamp; "
                "sys.exit(module['main']())"
            )
            args[1:2] = ["-c", clock_runner, str(PUBLISHER), recorded_at]
        return subprocess.run(args, env=self.env(), capture_output=True, text=True)

    def ckey(self):
        return f"catalogue/cadence/operator-host/{VERSION}.json"

    def okey(self, name="cadence-release.tar.gz"):
        return f"objects/cadence/operator-host/{VERSION}/{name}"

    def stored(self, key):
        f = self.store / ("obj__" + key.replace("/", "_"))
        return f.read_bytes() if f.exists() else None

    def entry(self):
        b = self.stored(self.ckey())
        return json.loads(b) if b else None

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def assert_aos_entry(self, entry):
        self.assertEqual(set(entry), ENTRY_FIELDS,
                         "catalogue entry does not match the settled AOS field set")
        self.assertEqual(entry["id"], f"cadence/operator-host/{VERSION}")
        self.assertEqual(entry["product"], "cadence")
        self.assertEqual(entry["profile"], "operator-host")
        self.assertEqual(entry["version"], VERSION)
        self.assertEqual(entry["sourceSha"], SOURCE_SHA)
        self.assertEqual(entry["visibility"], "private")
        self.assertEqual(entry["status"], "published")
        self.assertIsNone(entry["withdrawnAt"])
        self.assertRegex(entry["recordedAt"], ISO)
        self.assertLessEqual(len(entry["artifacts"]), 16)
        for a in entry["artifacts"]:
            self.assertEqual(a["profile"], "operator-host")
            self.assertRegex(a["sha256"], DIGEST)

    # POSITIVE ---------------------------------------------------------
    def test_first_publish_writes_objects_then_catalogue(self):
        r = self.publish()
        self.assertEqual(r.returncode, 0, f"first publish failed: {r.stderr}")
        out = json.loads(r.stdout)
        self.assertTrue(out["published"])
        self.assertEqual(out["status"], "published")
        entry = self.entry()
        self.assertIsNotNone(entry, "no catalogue record was written")
        self.assert_aos_entry(entry)
        obj = self.stored(self.okey())
        self.assertEqual(obj, self.artifact.read_bytes(),
                         "the stored artifact is not the handed source bytes")
        self.assertEqual(entry["artifacts"][0]["sha256"], f"sha256:{sha256(self.artifact)}")
        # catalogue written after its objects (the publish order guard):
        # the last put-object in the log is the catalogue key.
        puts = [c for c in self.calls() if c.startswith("put-object")]
        self.assertTrue(puts, "no objects were written")
        self.assertTrue(puts[-1].endswith(self.ckey()),
                        f"catalogue record was not written last: {puts}")

    # IDENTICAL RETRY --------------------------------------------------
    def test_identical_retry_preserves_first_metadata(self):
        r1 = self.publish(recorded_at="2033-05-18T03:33:20Z")
        self.assertEqual(r1.returncode, 0, f"first publish failed: {r1.stderr}")
        first = self.stored(self.ckey())
        first_entry = json.loads(first)
        r2 = self.publish(recorded_at="2033-05-18T03:33:21Z")
        self.assertEqual(r2.returncode, 0,
                         "an identical replay must be an idempotent success, not a "
                         f"conflict (exit {r2.returncode}): {r2.stderr}")
        self.assertEqual(self.stored(self.ckey()), first,
                         "identical replay rewrote the stored catalogue record — "
                         "the writer must preserve the first record's metadata")
        self.assertEqual(self.entry()["recordedAt"], first_entry["recordedAt"],
                         "identical replay stamped a new recordedAt")

    # CONFLICT ---------------------------------------------------------
    def test_changed_artifact_refuses_and_never_overwrites(self):
        r1 = self.publish()
        self.assertEqual(r1.returncode, 0, f"first publish failed: {r1.stderr}")
        first_obj = self.stored(self.okey())
        first_entry = self.stored(self.ckey())
        self.artifact.write_bytes(b"TAMPERED-TARBALL-BYTES-" + b"y" * 64)
        r2 = self.publish()
        self.assertNotEqual(r2.returncode, 0,
                            "a changed artifact for an existing id must refuse")
        self.assertEqual(self.stored(self.okey()), first_obj,
                         "the conflicting publish overwrote the stored object")
        self.assertEqual(self.stored(self.ckey()), first_entry,
                         "the conflicting publish overwrote the catalogue record")

    def test_changed_facts_refuse(self):
        r1 = self.publish()
        self.assertEqual(r1.returncode, 0, f"first publish failed: {r1.stderr}")
        first_entry = self.stored(self.ckey())
        # same artifact bytes, different supplied fact (candidate id)
        r2 = self.publish("--production-candidate-id", "111222333")
        self.assertNotEqual(r2.returncode, 0,
                            "a changed fact for an existing id must refuse")
        self.assertEqual(self.stored(self.ckey()), first_entry,
                         "a changed-facts publish overwrote the catalogue record")

    # UNSUPPORTED WITHDRAWAL -------------------------------------------
    def test_withdrawal_never_emits_a_published_record(self):
        r = self.publish("--status", "withdrawn",
                     "--withdrawn-at", "2026-10-15T00:00:00Z")
        # GREEN: unsupported withdrawal refuses before writing anything.
        # Acceptable GREEN: nothing written at all. The violation is a
        # PUBLISHED catalogue record left behind while reporting
        # withdrawn — that combination is what must never happen.
        self.assertIsNone(
            self.entry(),
            "a withdrawal invocation left a readable catalogue record — "
            "unsupported withdrawal must not emit published bytes")
        if r.returncode == 0:
            out = json.loads(r.stdout)
            self.assertNotEqual(
                out.get("status"), "withdrawn",
                "reported withdrawn while writing a published record")


if __name__ == "__main__":
    unittest.main(verbosity=2)
