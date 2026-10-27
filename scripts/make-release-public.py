#!/usr/bin/env python3
"""CAD-1475: make one already-published operator-host catalogue record public.

Explicit, operator-owned lifecycle step — NOT a republish. It exposes an
already-approved, byte-verified private release publicly without minting a
new binary, promotion or catalogue row. It does exactly one thing to
exactly one record:

  head-object captures the record's literal ETag; get-object --if-match
  <etag> reads the body conditioned on that same ETag (a record that moved
  between head and get refuses, not a foreign body); the closed schema and
  exact identity are validated; ONLY visibility flips private->public;
  put-object --if-match <etag> writes back — a provider-enforced CAS that
  fails PreconditionFailed if anything moved the object at any point.
  The reconcile readback is likewise --if-match on the NEW ETag the put
  returned, so it confirms our own write rather than a later one.

Safety shape:

- The endpoint and bucket are FIXED to the dedicated release store and
  are literals here; the --endpoint/--bucket arguments must equal them or
  the run refuses before any storage call.
- Credentials come only from the caller's AWS_* / AWS_PROFILE
  environment — never arguments, never output. A missing configuration
  fails `not_configured`.
- Conditional reads/writes send the provider's literal ETag (quotes
  included) verbatim; conditional support is asserted by the provider's
  own answer — a refused condition is a refusal, never a silent
  overwrite. No CAS is claimed from a local check.
- Every other record field is preserved; artifact objects are never read
  or written. Already-public is an honest no-op (exit 0).
- Missing/malformed/withdrawn/foreign records and stale-ETag conflicts
  refuse (exit 1). No blind put, retry-to-overwrite, delete, private or
  withdraw direction.
- Failures print one stable refusal code — never the service's raw
  stderr, never record contents, never caller values (a provider error
  string can carry auth identifiers).
- All reads are bounded: Content-Length is checked before the body is
  fetched, the fetch requests exactly that byte range, and every aws call
  runs under a finite timeout.

Usage:

  make-release-public.py \
    --endpoint https://87b59a40e130c425e35474687ad98a4f.r2.cloudflarestorage.com \
    --bucket agenticos-v2-release-artifacts \
    --source-sha <40-hex-sha>

Exit 0 = transitioned or honest noop; 1 = refused; 2 = usage error.
"""
import argparse
import datetime
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

# The dedicated release store this operation is bound to. Literals — the
# operation exists only for this account/bucket and never targets a
# caller-supplied store.
ENDPOINT = "https://87b59a40e130c425e35474687ad98a4f.r2.cloudflarestorage.com"
BUCKET = "agenticos-v2-release-artifacts"

FULL_SHA = re.compile(r"[0-9a-f]{40}")
ISO = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$")
VERSION = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,79}$")
ARTIFACT_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,159}$")
CONTENT_TYPES = ("application/octet-stream", "application/json", "text/plain")
PROFILES = ("operator-host", "public-client")
PRODUCTS = ("cadence", "image")
# The settled AOS-255 releaseEntrySchema field set — strict, no extras.
ENTRY_FIELDS = {
    "id",
    "product",
    "version",
    "sourceSha",
    "profile",
    "visibility",
    "status",
    "publishedAt",
    "withdrawnAt",
    "recordedAt",
    "productionCandidateId",
    "artifacts",
}
ARTIFACT_FIELDS = {"name", "profile", "platform", "sha256", "size", "contentType"}
DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
MAX_CATALOGUE_BYTES = 64 * 1024
MAX_ARTIFACTS = 16
AWS_TIMEOUT_SECS = 30


def fail(msg):
    # Refusals carry a stable code and a fixed description only — never
    # provider stderr, record contents or caller-supplied values.
    print(f"make-release-public: {msg}", file=sys.stderr)
    raise SystemExit(1)


class StorageError(Exception):
    """A failed storage call, classified internally; its text is never
    printed — callers turn it into a stable refusal."""

    def __init__(self, stderr):
        super().__init__()
        self.stderr = stderr

    def matches(self, *markers):
        return any(m in self.stderr for m in markers)


def is_iso_instant(value):
    """A real ISO-8601 instant: the format AND an actual calendar parse —
    a well-formed string with impossible fields (2026-99-99) is malformed."""
    if not isinstance(value, str) or not ISO.fullmatch(value):
        return False
    try:
        datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return False
    return True


def aws(args, *, env):
    """Run aws s3api with a finite timeout. A nonzero exit is a refusal
    carrying only a classified marker — stderr is read internally for the
    classification and never surfaced verbatim."""
    try:
        out = subprocess.run(
            ["aws", "s3api", *args],
            env=env,
            capture_output=True,
            text=True,
            check=False,
            timeout=AWS_TIMEOUT_SECS,
        )
    except FileNotFoundError:
        fail("refused:not_configured aws CLI is not on PATH")
    except subprocess.TimeoutExpired:
        fail("refused:storage_timeout aws call exceeded its bound")
    if out.returncode != 0:
        raise StorageError(out.stderr or "")
    return out


def head_object(key, env):
    """(etag, size) or None when absent. The ETag is the provider's literal
    quoted value, preserved for verbatim If-Match use."""
    try:
        out = aws(
            [
                "head-object",
                "--bucket",
                BUCKET,
                "--key",
                key,
                "--endpoint-url",
                ENDPOINT,
            ],
            env=env,
        )
    except StorageError as e:
        if e.matches("404", "Not Found", "NoSuchKey"):
            return None
        raise
    try:
        meta = json.loads(out.stdout)
    except ValueError:
        fail("refused:storage_malformed head-object returned unparseable metadata")
    etag = meta.get("ETag")
    if not isinstance(etag, str) or not etag:
        fail("refused:storage_malformed catalogue record has no usable ETag")
    size = meta.get("ContentLength")
    if not isinstance(size, int) or isinstance(size, bool) or size < 0:
        fail("refused:storage_malformed catalogue record has no usable ContentLength")
    return etag, size


def get_object(key, etag, size, dest, env):
    """Fetch record bytes conditioned on `etag` and bounded to exactly
    `size` bytes via --range — a moved record or an oversized body refuses
    rather than being trusted under a stale or unbounded read."""
    try:
        aws(
            [
                "get-object",
                "--bucket",
                BUCKET,
                "--key",
                key,
                "--if-match",
                etag,
                "--range",
                f"bytes=0-{size - 1}",
                "--endpoint-url",
                ENDPOINT,
                str(dest),
            ],
            env=env,
        )
    except StorageError as e:
        if e.matches("PreconditionFailed", "412"):
            fail(
                "refused:etag_conflict catalogue record changed between reads — "
                "refusing rather than trusting a moved record"
            )
        fail("refused:storage_read catalogue record could not be fetched")
    if dest.stat().st_size != size:
        fail("refused:storage_malformed fetched record length differs from its metadata")


def put_if_match(key, path, etag, env):
    """CAS write: replace the record only if its current ETag is still the
    exact one captured. Returns the NEW literal ETag on success, False on
    a refused condition — a conflict refusal is never retried."""
    try:
        out = aws(
            [
                "put-object",
                "--bucket",
                BUCKET,
                "--key",
                key,
                "--body",
                str(path),
                "--content-type",
                "application/json",
                "--if-match",
                etag,
                "--endpoint-url",
                ENDPOINT,
            ],
            env=env,
        )
    except StorageError as e:
        if e.matches("PreconditionFailed", "412"):
            return False
        fail("refused:storage_write conditional write failed")
    try:
        new_etag = json.loads(out.stdout).get("ETag")
    except ValueError:
        new_etag = None
    if not isinstance(new_etag, str) or not new_etag:
        fail("refused:storage_malformed conditional write returned no ETag")
    return new_etag


def validate_entry(record):
    """Closed validation of one catalogue document — returns a refusal code
    or None. Mirrors the settled releaseEntrySchema field set so a
    malformed or fattened record refuses rather than being trusted."""
    if not isinstance(record, dict):
        return "malformed:not_object"
    if set(record) != ENTRY_FIELDS:
        return "malformed:schema"
    if not isinstance(record["id"], str) or not (1 <= len(record["id"]) <= 240):
        return "malformed:id"
    if record["product"] not in PRODUCTS:
        return "malformed:product"
    if record["profile"] not in PROFILES:
        return "malformed:profile"
    version = record["version"]
    if (
        not isinstance(version, str)
        or not VERSION.fullmatch(version)
        or ".." in version
        or set(version) == {"."}
    ):
        return "malformed:version"
    if record["id"] != f"{record['product']}/{record['profile']}/{record['version']}":
        return "malformed:id"
    if not isinstance(record["sourceSha"], str) or not FULL_SHA.fullmatch(record["sourceSha"]):
        return "malformed:sourceSha"
    if record["visibility"] not in ("public", "private"):
        return "malformed:visibility"
    if record["status"] not in ("published", "withdrawn"):
        return "malformed:status"
    for field in ("publishedAt", "withdrawnAt"):
        if record[field] is not None and not is_iso_instant(record[field]):
            return "malformed:date"
    if not is_iso_instant(record["recordedAt"]):
        return "malformed:date"
    pcid = record["productionCandidateId"]
    if pcid is not None and not (isinstance(pcid, str) and 1 <= len(pcid) <= 200):
        return "malformed:productionCandidateId"
    arts = record["artifacts"]
    if not isinstance(arts, list) or len(arts) > MAX_ARTIFACTS:
        return "malformed:artifacts"
    names = set()
    for art in arts:
        if not isinstance(art, dict) or set(art) != ARTIFACT_FIELDS:
            return "malformed:artifact"
        if not isinstance(art["name"], str) or not ARTIFACT_NAME.fullmatch(art["name"]):
            return "malformed:artifact_name"
        if art["name"] in names:
            return "malformed:artifact_name"
        names.add(art["name"])
        if art["profile"] != record["profile"]:
            return "malformed:artifact_profile"
        if not isinstance(art["platform"], str) or not (1 <= len(art["platform"]) <= 64):
            return "malformed:artifact_platform"
        if not isinstance(art["sha256"], str) or not DIGEST.fullmatch(art["sha256"]):
            return "malformed:artifact_sha256"
        # bool is an int in Python — a true/false size must refuse, not pass.
        if not isinstance(art["size"], int) or isinstance(art["size"], bool) or art["size"] < 0:
            return "malformed:artifact_size"
        if art["contentType"] not in CONTENT_TYPES:
            return "malformed:artifact_contentType"
    if record["status"] == "withdrawn" and record["withdrawnAt"] is None:
        return "malformed:withdrawn"
    return None


def canonical_bytes(record):
    """The same fixed serialization the publisher writes — sorted keys,
    compact separators — so the byte readback compares clean."""
    return json.dumps(record, separators=(",", ":"), sort_keys=True).encode()


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--endpoint", required=True, help="The dedicated endpoint literal.")
    parser.add_argument("--bucket", required=True, help="The dedicated bucket literal.")
    parser.add_argument(
        "--source-sha",
        required=True,
        help="Full lowercase 40-hex source commit whose operator-host record goes public.",
    )
    args = parser.parse_args()

    if args.endpoint != ENDPOINT or args.bucket != BUCKET:
        fail("refused:foreign_store this operation targets only the dedicated release store")
    if not FULL_SHA.fullmatch(args.source_sha):
        fail("usage --source-sha must be a full lowercase 40-hex commit")

    env = dict(os.environ)
    if not (
        env.get("AWS_PROFILE")
        or (env.get("AWS_ACCESS_KEY_ID") and env.get("AWS_SECRET_ACCESS_KEY"))
        or env.get("AWS_WEB_IDENTITY_TOKEN_FILE")
    ):
        fail("refused:not_configured no AWS credentials in the environment")

    key = f"catalogue/cadence/operator-host/{args.source_sha}.json"

    # ---- capture the record's actual ETag + bounded size first ----
    try:
        head = head_object(key, env)
    except StorageError:
        fail("refused:storage_read catalogue metadata could not be read")
    if head is None:
        fail("refused:not_found no catalogue record for this cadence/operator-host source")
    etag, size = head
    if size < 1 or size > MAX_CATALOGUE_BYTES:
        fail("refused:oversized catalogue record exceeds the bounded size")

    with tempfile.TemporaryDirectory() as td:
        got = Path(td) / "entry.json"
        get_object(key, etag, size, got, env)
        try:
            record = json.loads(got.read_bytes())
        except (ValueError, UnicodeDecodeError):
            fail("refused:malformed catalogue record is not valid JSON")
        reason = validate_entry(record)
        if reason is not None:
            fail(f"refused:{reason}")

        # Bind to the requested subject: the record must be exactly the
        # cadence/operator-host/<source-sha> entry named on the CLI.
        if (
            record["product"] != "cadence"
            or record["profile"] != "operator-host"
            or record["version"] != args.source_sha
            or record["sourceSha"] != args.source_sha
        ):
            fail("refused:foreign_record catalogue record is not the requested subject")
        if record["status"] != "published" or record["withdrawnAt"] is not None:
            fail("refused:closed_record withdrawn records are never published by this path")

        if record["visibility"] == "public":
            print(
                json.dumps(
                    {
                        "ok": True,
                        "noop": True,
                        "id": record["id"],
                        "visibility": "public",
                        "detail": "already public; no write",
                    },
                    indent=2,
                )
            )
            return

        # The only mutation: visibility private -> public.
        updated = dict(record)
        updated["visibility"] = "public"
        updated_bytes = canonical_bytes(updated)

        out_file = Path(td) / "entry-public.json"
        out_file.write_bytes(updated_bytes)

        new_etag = put_if_match(key, out_file, etag, env)
        if not new_etag:
            fail(
                "refused:etag_conflict catalogue record changed between read and "
                "conditional write — refusing; re-run to re-read"
            )

        # Reconcile: read back conditioned on the NEW ETag the write
        # returned — the stored record must equal our own write, and a
        # record that already moved again refuses rather than passing a
        # foreign body as ours.
        readback = Path(td) / "readback.json"
        get_object(key, new_etag, len(updated_bytes), readback, env)
        if readback.read_bytes() != updated_bytes:
            fail("refused:reconcile the updated catalogue record does not match what was written")

    print(
        json.dumps(
            {
                "ok": True,
                "noop": False,
                "id": record["id"],
                "visibility": "private -> public",
                "key": key,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
