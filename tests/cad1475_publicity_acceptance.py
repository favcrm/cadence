#!/usr/bin/env python3
"""CAD-1475 INDEPENDENT publicity lifecycle acceptance — conditional flip only.

Authored independently of the implementer; the implementer may not edit
or weaken this file.

This runs the REAL `scripts/make-release-public.py` end to end as a
subprocess against an isolated local `aws` shim that models ONLY the
s3api surface the lifecycle operation needs (get-object, head-object,
put-object with a REAL --if-match conditional) over an on-disk
namespace — no credentials, no network, no real R2. The shim is
transparent, logging plumbing: it records every invocation (argv-level)
so the check asserts what the helper actually asked storage to do —
including the exact If-Match value sent — not a fabricated outcome.

The seeded record mirrors the FIELD SHAPE of the settled AOS release
contract (the published-catalogue.json shape for run 38824540457) with
synthetic names/digests — the fixture carries no real source, artifact
or secret.

What it proves (the ticket's lifecycle guards):

  POSITIVE — a private, published, schema-valid entry flips to
    `visibility: "public"` and NOTHING else: every other field
    (id/sourceSha/dates/candidate/artifact digests+size) is preserved
    byte-for-byte. The conditional write sends the catalogue object's
    actual captured ETag via --if-match, and no artifact object key is
    ever written — only the catalogue record is put, once.

  NO-OP — an already-public entry exits 0, reports an honest no-op and
    performs NO put at all (no blind write to rewrite identical bytes).

  STALE-ETag — a record that changed between the helper's read and its
    conditional write is refused: storage answers PreconditionFailed,
    the exit is non-zero, and the changed stored record is left
    untouched. There is no retry-to-overwrite after the refusal.

  CLOSED RECORDS — a withdrawn entry, a malformed (unparseable)
    catalogue document, and a record whose sourceSha does not equal the
    requested --source-sha all refuse with NO write of any kind; the
    stored bytes stay identical.

  SCOPE — an endpoint or bucket other than the fixed dedicated
    release store is refused before any storage call; the operation
    never reads or writes an arbitrary S3 target it was handed.

  CAPTURED BODY — the read of the catalogue record is itself bound to
    the captured ETag (a real conditional `get-object --if-match`, the
    same conditional an unbounded get cannot give); a record that moves
    between the ETag capture and the body read is refused as the same
    concurrent-write conflict, never silently published. The read is
    bounded (`--range` under the catalogue size cap), so a corrupted or
    oversized body can never be trusted.

  READ-ONLY BOUNDARY — across every run, the only key ever written is
    the catalogue record for the exact requested identity; object keys
    under objects/ are never touched by the lifecycle operation.

LIMITS: local deterministic S3 semantics only. Real R2 conditional
support, account IAM, credential custody, partial-failure windows and
the operator dispatch path remain UNQUALIFIED — this shim is
transparent plumbing evidence, not live CAS/authority qualification.
"""
import hashlib
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(
    os.environ.get("CAD1475_REPO", Path(__file__).resolve().parents[1])
)
HELPER = REPO / "scripts" / "make-release-public.py"

SOURCE_SHA = "2" * 40
VERSION = SOURCE_SHA  # operator-host catalogue records are keyed by source sha
ENDPOINT = "https://sham.local.r2.invalid"
BUCKET = "releases-test"
PRODUCT = "cadence"
PROFILE = "operator-host"

CATALOGUE_KEY = f"catalogue/{PRODUCT}/{PROFILE}/{VERSION}.json"
ARTIFACT_NAME = "cadence-release.tar.gz"
ARTIFACT_KEY = f"objects/{PRODUCT}/{PROFILE}/{VERSION}/{ARTIFACT_NAME}"

ARTIFACT_BYTES = b"FIXTURE-TARBALL-BYTES-" + bytes(range(64))


def base_entry(visibility="private", status="published", withdrawn_at=None):
    """A synthetic record with the settled AOS catalogue field shape."""
    return {
        "artifacts": [
            {
                "contentType": "application/octet-stream",
                "name": ARTIFACT_NAME,
                "platform": "x86_64-linux",
                "profile": PROFILE,
                "sha256": f"sha256:{hashlib.sha256(ARTIFACT_BYTES).hexdigest()}",
                "size": len(ARTIFACT_BYTES),
            }
        ],
        "id": f"{PRODUCT}/{PROFILE}/{VERSION}",
        "product": PRODUCT,
        "productionCandidateId": "988112233",
        "profile": PROFILE,
        "publishedAt": None,
        "recordedAt": "2026-10-16T03:59:29Z",
        "sourceSha": SOURCE_SHA,
        "status": status,
        "version": VERSION,
        "visibility": visibility,
        "withdrawnAt": withdrawn_at,
    }


# The local `aws` shim. Implements get-object, head-object and put-object
# with a REAL --if-match conditional over an on-disk store; appends one
# JSON line per invocation to $SHIM_LOG so the test can assert the exact
# operation, key, if-match value and whether a body was written. The
# store dir holds obj__<key> (bytes) and obj__<key>.meta (JSON with the
# stored ETag). A put with --if-match whose value does not equal the
# stored ETag refuses with PreconditionFailed — the same refusal a real
# conditional write surfaces.
SHIM = r'''#!/usr/bin/env python3
import hashlib, json, os, sys
store = os.environ["SHIM_STORE"]; os.makedirs(store, exist_ok=True)
log = os.environ.get("SHIM_LOG")

def kf(key): return os.path.join(store, "obj__" + key.replace("/", "_"))
def mf(key): return kf(key) + ".meta"
def stored_etag(key):
    if not os.path.exists(mf(key)): return None
    return json.loads(open(mf(key)).read()).get("ETag")

args = sys.argv[1:]
if len(args) < 2 or args[0] != "s3api":
    print("shim: expected s3api", file=sys.stderr); sys.exit(2)
op = args[1]

def maybe_move(key):
    # Simulated concurrent writer: when SHIM_MOVE_FILE/SHIM_MOVE_KEY are
    # set, the FIRST call on that key that matches SHIM_MOVE_ON
    # (default "get-object") is answered normally and then a competing
    # write lands — the stored object and its ETag are replaced, so a
    # later conditional read or write carrying the pre-move ETag
    # honestly fails PreconditionFailed. The marker file is consumed so
    # the move happens exactly once.
    move_file = os.environ.get("SHIM_MOVE_FILE")
    on = os.environ.get("SHIM_MOVE_ON", "get-object")
    if move_file and os.environ.get("SHIM_MOVE_KEY") == key and op == on \
            and os.path.exists(move_file):
        moved = open(move_file, "rb").read()
        open(kf(key), "wb").write(moved)
        open(mf(key), "w").write(json.dumps({
            "ETag": '"%s"' % hashlib.sha256(moved).hexdigest(),
            "ContentLength": len(moved)}))
        os.unlink(move_file)
def val(flag):
    i = args.index(flag); return args[i + 1]
key = val("--key") if "--key" in args else ""
im = val("--if-match") if "--if-match" in args else None
inm = val("--if-none-match") if "--if-none-match" in args else None
if log:
    with open(log, "a") as f:
        f.write(json.dumps({"op": op, "key": key,
                            "if_match": im, "if_none_match": inm}) + "\n")

if op == "get-object":
    dest = args[-1]
    if not os.path.exists(kf(key)):
        print("An error occurred (NoSuchKey) when calling GetObject", file=sys.stderr); sys.exit(1)
    data = open(kf(key), "rb").read()
    # Conditional read: a real `get-object --if-match` refuses when the
    # stored ETag no longer equals the captured one — the caller never
    # sees a body that moved since the read it based its decision on.
    if im is not None:
        cur = stored_etag(key)
        if cur is None or cur.strip('"') != im.strip('"'):
            print("An error occurred (PreconditionFailed) when calling GetObject: if-match",
                  file=sys.stderr); sys.exit(1)
    # Bounded read: `--range bytes=A-B` serves only that byte window —
    # the same bounded read a capped catalogue allows, so a corrupted
    # oversized body is never trusted past the cap.
    if "--range" in args:
        rng = val("--range")
        m = __import__("re").fullmatch(r"bytes=(\d+)-(\d*)", rng)
        if m:
            lo, hi = int(m.group(1)), (int(m.group(2)) if m.group(2) else len(data) - 1)
            data = data[lo:hi + 1]
    open(dest, "wb").write(data)
    print(json.dumps({"ContentLength": len(data),
                      "ETag": stored_etag(key) or ""}))
    maybe_move(key)
elif op == "head-object":
    if not os.path.exists(mf(key)):
        print("An error occurred (404) when calling HeadObject: Not Found", file=sys.stderr); sys.exit(1)
    print(open(mf(key)).read())
    maybe_move(key)
elif op == "put-object":
    if inm == "*" and os.path.exists(kf(key)):
        print("An error occurred (PreconditionFailed) when calling PutObject: if-none-match",
              file=sys.stderr); sys.exit(1)
    if im is not None:
        cur = stored_etag(key)
        # The real s3api --if-match compares the caller value against the
        # object's current ETag whether the caller quotes it or not —
        # normalize the stored quoted form for a fair comparison.
        if cur is None or cur.strip('"') != im.strip('"'):
            print("An error occurred (PreconditionFailed) when calling PutObject: if-match",
                  file=sys.stderr); sys.exit(1)
    data = open(val("--body"), "rb").read()
    open(kf(key), "wb").write(data)
    etag = '"%s"' % hashlib.sha256(data).hexdigest()
    open(mf(key), "w").write(json.dumps({
        "ETag": etag,
        "ContentLength": len(data),
        "ContentType": val("--content-type") if "--content-type" in args else ""}))
    print(json.dumps({"ETag": etag}))
else:
    print("shim: unsupported op " + op, file=sys.stderr); sys.exit(2)
'''


def entry_bytes(entry):
    return json.dumps(entry, separators=(",", ":"), sort_keys=True).encode()


class PublicityCase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="c1475-pub-")
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

    def env(self):
        e = dict(os.environ)
        e["PATH"] = f"{self.tools}:{e['PATH']}"
        e["SHIM_STORE"] = str(self.store)
        e["SHIM_LOG"] = str(self.log)
        # Dummy credential env satisfies the helper's own not_configured
        # gate; the shim never reads them.
        e["AWS_ACCESS_KEY_ID"] = "sham-key-id"
        e["AWS_SECRET_ACCESS_KEY"] = "sham-secret"
        e.pop("AWS_PROFILE", None)
        return e

    # The fixed dedicated store the publicity operation is scoped to.
    # Any other endpoint/bucket must refuse before reaching storage.
    EXPECTED_ENDPOINT = "https://87b59a40e130c425e35474687ad98a4f.r2.cloudflarestorage.com"
    EXPECTED_BUCKET = "agenticos-v2-release-artifacts"

    def run_helper(self, *extra, extra_env=None):
        args = [sys.executable, str(HELPER),
                "--endpoint", self.EXPECTED_ENDPOINT,
                "--bucket", self.EXPECTED_BUCKET,
                "--source-sha", SOURCE_SHA]
        args += list(extra)
        env = self.env()
        if extra_env:
            env.update(extra_env)
        return subprocess.run(args, env=env, capture_output=True, text=True)

    def seed(self, key, data):
        f = self.store / ("obj__" + key.replace("/", "_"))
        f.write_bytes(data if isinstance(data, bytes) else data.encode())
        meta = {"ETag": '"%s"' % hashlib.sha256(f.read_bytes()).hexdigest(),
                "ContentLength": f.stat().st_size}
        (self.store / ("obj__" + key.replace("/", "_") + ".meta")).write_text(
            json.dumps(meta))
        return meta["ETag"]

    def stored(self, key):
        f = self.store / ("obj__" + key.replace("/", "_"))
        return f.read_bytes() if f.exists() else None

    def stored_etag(self, key):
        m = self.store / ("obj__" + key.replace("/", "_") + ".meta")
        return json.loads(m.read_text())["ETag"] if m.exists() else None

    def calls(self):
        if not self.log.exists():
            return []
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def seed_entry(self, **kw):
        entry = base_entry(**kw)
        raw = entry_bytes(entry)
        etag = self.seed(CATALOGUE_KEY, raw)
        return entry, raw, etag

    # POSITIVE -------------------------------------------------------
    def test_private_entry_flips_to_public_preserving_every_other_field(self):
        entry, raw, etag = self.seed_entry()
        self.seed(ARTIFACT_KEY, ARTIFACT_BYTES)
        r = self.run_helper()
        self.assertEqual(r.returncode, 0, f"publicity flip failed: {r.stderr}")
        out = json.loads(r.stdout)
        # Honest outcome signal — the exact key names are the helper's,
        # but a successful flip must say so plainly.
        self.assertTrue(out.get("ok") is True,
                        f"stdout did not report a successful publicity transition: {out}")
        self.assertFalse(out.get("noop", False),
                         f"a private record reported a noop: {out}")
        stored = self.stored(CATALOGUE_KEY)
        self.assertIsNotNone(stored, "no catalogue record remains stored")
        new_entry = json.loads(stored)
        self.assertEqual(new_entry["visibility"], "public")
        # Every other field preserved EXACTLY.
        for field in (
            "id", "product", "version", "sourceSha", "profile", "status",
            "publishedAt", "withdrawnAt", "recordedAt",
            "productionCandidateId", "artifacts",
        ):
            self.assertEqual(
                new_entry[field], entry[field],
                f"field {field} changed during a visibility-only flip",
            )
        self.assertEqual(set(new_entry), set(entry),
                         "the field set itself changed during the flip")
        # The conditional write carried the actual captured ETag.
        puts = [c for c in self.calls() if c["op"] == "put-object"]
        self.assertEqual(len(puts), 1, f"expected exactly one put-object: {puts}")
        self.assertEqual(puts[0]["key"], CATALOGUE_KEY)
        # The put must carry the EXACT captured ETag verbatim (including
        # the provider's quoting) — not a stripped or recomputed value.
        self.assertEqual(
            puts[0]["if_match"], etag,
            "the conditional write did not send the captured ETag verbatim",
        )
        # Artifact objects are never written by the lifecycle operation.
        self.assertFalse(
            any(c["op"] == "put-object" and c["key"] != CATALOGUE_KEY
                for c in self.calls()),
            "the lifecycle operation wrote a non-catalogue key",
        )
        self.assertEqual(self.stored(ARTIFACT_KEY), ARTIFACT_BYTES,
                         "the stored artifact object was touched")

    def test_already_public_is_an_honest_noop_with_no_write(self):
        entry, raw, _etag = self.seed_entry(visibility="public")
        r = self.run_helper()
        self.assertEqual(r.returncode, 0,
                         f"an already-public record must be a no-op success: {r.stderr}")
        self.assertEqual(self.stored(CATALOGUE_KEY), raw,
                         "an already-public no-op rewrote the record")
        self.assertFalse(
            any(c["op"] == "put-object" for c in self.calls()),
            "an already-public record must not be put again",
        )

    # SCOPE — the fixed dedicated store --------------------------------
    def test_wrong_endpoint_refuses_before_any_storage_call(self):
        self.seed_entry()
        # Passing a DIFFERENT https endpoint must refuse before touching
        # storage — the operation is bound to the dedicated release store
        # literal, so a foreign endpoint never reaches an aws call.
        before = len(self.calls())
        bad = subprocess.run(
            [sys.executable, str(HELPER),
             "--endpoint", "https://attacker.example.invalid",
             "--bucket", self.EXPECTED_BUCKET,
             "--source-sha", SOURCE_SHA],
            env=self.env(), capture_output=True, text=True)
        self.assertNotEqual(bad.returncode, 0,
                            "a non-fixed endpoint must refuse")
        self.assertEqual(len(self.calls()), before,
                         "a foreign endpoint reached a storage call: "
                         f"{self.calls()[before:]}")

    def test_wrong_bucket_refuses_before_any_storage_call(self):
        self.seed_entry()
        before = len(self.calls())
        bad = subprocess.run(
            [sys.executable, str(HELPER),
             "--endpoint", self.EXPECTED_ENDPOINT,
             "--bucket", "other-bucket",
             "--source-sha", SOURCE_SHA],
            env=self.env(), capture_output=True, text=True)
        self.assertNotEqual(bad.returncode, 0,
                            "a non-fixed bucket must refuse")
        self.assertEqual(len(self.calls()), before,
                         "a foreign bucket reached a storage call: "
                         f"{self.calls()[before:]}")

    # CAPTURED BODY — the read is bound to the captured ETag -----------
    def test_record_read_is_conditional_and_bounded(self):
        _entry, _raw, etag = self.seed_entry()
        r = self.run_helper()
        self.assertEqual(r.returncode, 0, f"positive flip failed: {r.stderr}")
        gets = [c for c in self.calls() if c["op"] == "get-object"
                and c["key"] == CATALOGUE_KEY]
        self.assertTrue(gets, "no catalogue get-object was logged")
        # The decision-forming read (the FIRST catalogue get, before any
        # write) must carry the exact captured ETag verbatim — an
        # unconditional get cannot detect a body that moved after the
        # decision to publish. A post-write readback is unconditional by
        # design, so scope the assertion to the read that precedes a put.
        put_index = next(
            (i for i, c in enumerate(self.calls()) if c["op"] == "put-object"),
            len(self.calls()),
        )
        decision_gets = [
            c for i, c in enumerate(self.calls())
            if c["op"] == "get-object" and c["key"] == CATALOGUE_KEY
            and i < put_index
        ]
        self.assertTrue(decision_gets,
                        "no decision-forming conditional catalogue read was logged")
        for g in decision_gets:
            self.assertIsNotNone(g["if_match"],
                                 "catalogue get-object carried no --if-match")
            self.assertEqual(g["if_match"], etag,
                             "catalogue read was not bound to the captured ETag verbatim")

    def test_oversized_record_refuses_before_body_read(self):
        # A catalogue object whose ContentLength exceeds the bounded cap
        # must refuse at the head — the body is never fetched, and no
        # unbounded read can be coerced into trusting an oversized blob.
        entry, _raw, _etag = self.seed_entry()
        fat = dict(entry)
        raw = entry_bytes(fat)
        # Seed with a corrupted oversized ContentLength on the meta.
        f = self.store / ("obj__" + CATALOGUE_KEY.replace("/", "_"))
        (self.store / ("obj__" + CATALOGUE_KEY.replace("/", "_") + ".meta")).write_text(
            json.dumps({"ETag": self.stored_etag(CATALOGUE_KEY),
                        "ContentLength": 700 * 1024}))
        r = self.run_helper()
        self.assertNotEqual(r.returncode, 0,
                            "an oversized catalogue record must refuse")
        self.assertFalse(
            any(c["op"] == "get-object" for c in self.calls()),
            "an oversized record still had its body fetched",
        )
        self.assertFalse(
            any(c["op"] == "put-object" for c in self.calls()),
            "an oversized record still wrote",
        )

    def test_mid_read_move_refuses_and_never_publishes(self):
        entry, _raw, _etag = self.seed_entry()
        # The record moves BETWEEN the head-object ETag capture and the
        # body get: the conditional get now sees a changed ETag and the
        # whole operation must refuse without any write — an ABA conflict
        # an unconditional read cannot catch.
        moved = dict(entry)
        moved["visibility"] = "private"  # still private; only the write matters
        moved["recordedAt"] = "2026-10-16T04:00:00Z"
        move_file = self.dir / "mid-read-move.bin"
        move_file.write_bytes(entry_bytes(moved))
        r = self.run_helper(
            extra_env={"SHIM_MOVE_FILE": str(move_file),
                       "SHIM_MOVE_KEY": CATALOGUE_KEY,
                       # The move lands right after the ETag capture, so
                       # the body read hits the moved record.
                       "SHIM_MOVE_ON": "head-object"})
        self.assertNotEqual(r.returncode, 0,
                            "a record that moved mid-read must refuse: "
                            f"{r.stdout} {r.stderr}")
        self.assertFalse(
            any(c["op"] == "put-object" for c in self.calls()),
            "a mid-read move still wrote: " f"{self.calls()}")

    # CLOSED / REFUSAL -------------------------------------------------
    def test_stale_etag_conflict_refuses_and_never_overwrites(self):
        entry, _raw, _etag = self.seed_entry()
        # A competing writer lands between the helper's read and its
        # conditional put: the get serves the original record, then the
        # shim swaps in a CHANGED record with a fresh ETag. The helper's
        # captured ETag is stale at write time — a real CAS refusal.
        moved = dict(entry)
        moved["recordedAt"] = "2026-10-16T04:00:00Z"
        moved_raw = entry_bytes(moved)
        move_file = self.dir / "concurrent-write.bin"
        move_file.write_bytes(moved_raw)
        r = self.run_helper(
            extra_env={"SHIM_MOVE_FILE": str(move_file),
                       "SHIM_MOVE_KEY": CATALOGUE_KEY})
        self.assertNotEqual(r.returncode, 0,
                            "a stale-ETag conditional write must refuse: "
                            f"{r.stdout} {r.stderr}")
        # The stored record is the competing writer's — NOT the helper's
        # public version of the stale read.
        self.assertEqual(self.stored(CATALOGUE_KEY), moved_raw,
                         "a refused conditional write overwrote a moved record")
        stored_entry = json.loads(self.stored(CATALOGUE_KEY))
        self.assertEqual(stored_entry["visibility"], "private",
                         "the moved record was published despite the conflict")
        puts = [c for c in self.calls() if c["op"] == "put-object"]
        # The refused put is the only put — no blind retry-to-overwrite.
        self.assertEqual(len(puts), 1,
                         f"the helper retried the write after refusal: {puts}")

    def test_withdrawn_record_refuses_with_no_write(self):
        _entry, raw, _etag = self.seed_entry(
            status="withdrawn", withdrawn_at="2026-10-17T00:00:00Z")
        r = self.run_helper()
        self.assertNotEqual(r.returncode, 0,
                            "a withdrawn record must refuse publicity")
        self.assertEqual(self.stored(CATALOGUE_KEY), raw,
                         "a refused withdrawn record was overwritten")
        self.assertFalse(
            any(c["op"] == "put-object" for c in self.calls()),
            "a withdrawn refusal must not write",
        )

    def test_malformed_record_refuses_with_no_write(self):
        self.seed(CATALOGUE_KEY, b"{not valid json")
        r = self.run_helper()
        self.assertNotEqual(r.returncode, 0,
                            "a malformed catalogue record must refuse")
        self.assertFalse(
            any(c["op"] == "put-object" for c in self.calls()),
            "a malformed record must never be written over",
        )

    def test_foreign_record_refuses_with_no_write(self):
        # The stored record sits at the requested subject's key but its
        # own fields name a different source — a foreign/misplaced record
        # the helper must refuse rather than publish.
        entry, _raw, _etag = self.seed_entry()
        foreign = dict(entry)
        foreign["sourceSha"] = "3" * 40
        raw = entry_bytes(foreign)
        self.seed(CATALOGUE_KEY, raw)
        r = self.run_helper()
        self.assertNotEqual(r.returncode, 0,
                            "a record whose sourceSha differs must refuse")
        self.assertEqual(self.stored(CATALOGUE_KEY), raw)
        self.assertFalse(
            any(c["op"] == "put-object" for c in self.calls()),
            "a foreign record must never be written over",
        )

    def test_missing_record_refuses(self):
        r = self.run_helper()
        self.assertNotEqual(r.returncode, 0,
                            "a missing catalogue record must refuse")
        self.assertFalse(
            any(c["op"] == "put-object" for c in self.calls()),
            "a missing record must never be created by the publicity op",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
