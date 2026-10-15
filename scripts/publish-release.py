#!/usr/bin/env python3
"""CAD-1455: publish an already-gated release into owned immutable storage.

This is the Cadence-side publisher for the AOS-255 release data plane.
It runs only after the existing gates that produced the release —
the completed production promotion (operator-host) or the published
public release (public-client). It copies the EXACT original bytes it
is handed (never rebuilds, never re-attests), writes every artifact
object under `objects/<product>/<profile>/<version>/<name>` with a
conditional immutable create, and writes the catalogue record LAST at
`catalogue/<product>/<profile>/<version>.json` only after every object
is verified present with the recorded bytes.

Safety shape:

- Storage is addressed by `--endpoint`, `--bucket` and
  `AWS_PROFILE`/`AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY` in the
  environment (or a shared config profile). No credential, token or
  secret is ever taken as an argument or printed. A missing
  configuration fails honestly (`not_configured`) rather than guessing.
- Every object write is `put_object ... --if-none-match '*'` — a
  conditional create. A name that already exists is never overwritten;
  it is accepted only when a byte-for-byte comparison of the existing
  object equals the bytes this run would write (idempotent retry), and
  any different content is a conflict that refuses, never repairs.
- The catalogue entry is written last and itself conditional: a
  conflicting existing entry for the same identity refuses rather than
  replacing approved metadata.
- After the write the script reads back the catalogue object and
  compares it byte-for-byte, then HEADs every artifact for its
  recorded size. A partial or unknown outcome reports `held`, not
  success; nothing is announced blindly.
- `visibility` is explicit and defaults to `private`. `operator-host`
  publication is private unless `--visibility public` is given; a
  `public-client` record may be public only when the caller asks for
  it. The public data plane answers private entries as absent.

Usage (all inputs are exact existing bytes; nothing is fetched or built):

  publish-release.py \
    --endpoint https://<account>.r2.cloudflarestorage.com \
    --bucket <bucket> \
    --product cadence --profile operator-host --version <40-hex-sha> \
    --source-sha <40-hex-sha> \
    --visibility private \
    --production-candidate-id <staging-run-id> \
    --artifact NAME=PATH[:PLATFORM:CONTENT_TYPE] ...

`--dry-run` validates inputs and prints the planned keys without
touching storage. Exit 0 published/verified, 1 refused, 2 usage.
"""
import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

FULL_SHA = re.compile(r"[0-9a-f]{40}")
VERSION = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]{0,79}$")
ARTIFACT_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,159}$")
ISO = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$")
PROFILES = ("operator-host", "public-client")
CONTENT_TYPES = ("application/octet-stream", "application/json", "text/plain")
MAX_ARTIFACTS = 16
MAX_OBJECT_BYTES = 768 * 1024 * 1024


def sha256_of(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def fail(msg):
    print(f"publish-release: {msg}", file=sys.stderr)
    raise SystemExit(1)


def aws(args, *, env, capture=True, binary=False):
    """Run aws s3api; every failure is a refusal with stderr, never a guess."""
    cmd = ["aws", "s3api", *args]
    try:
        out = subprocess.run(
            cmd, env=env, capture_output=capture, text=not binary, check=False
        )
    except FileNotFoundError:
        fail("aws CLI is not on PATH — install it or publish through the operator's own tooling")
    if out.returncode != 0:
        stderr = out.stderr if isinstance(out.stderr, str) else out.stderr.decode()
        raise RuntimeError(stderr.strip() or f"aws exited {out.returncode}")
    return out


def head_object(endpoint, bucket, key, env):
    """Return the object's (etag, size) or None when absent."""
    try:
        out = aws(
            ["head-object", "--bucket", bucket, "--key", key, "--endpoint-url", endpoint],
            env=env,
        )
    except RuntimeError as e:
        text = str(e)
        if "404" in text or "Not Found" in text or "NoSuchKey" in text:
            return None
        raise
    meta = json.loads(out.stdout)
    return meta.get("ETag", "").strip('"'), meta.get("ContentLength")


def put_new(endpoint, bucket, key, path, content_type, env):
    """Conditional create: refuse (PreconditionFailed) rather than overwrite."""
    try:
        aws(
            [
                "put-object",
                "--bucket", bucket,
                "--key", key,
                "--body", str(path),
                "--content-type", content_type,
                "--if-none-match", "*",
                "--endpoint-url", endpoint,
            ],
            env=env,
        )
        return True
    except RuntimeError as e:
        if "PreconditionFailed" in str(e) or "412" in str(e):
            return False
        raise


def download(endpoint, bucket, key, dest, env):
    aws(
        ["get-object", "--bucket", bucket, "--key", key,
         "--endpoint-url", endpoint, str(dest)],
        env=env,
    )


def entry_document(args, artifacts):
    """The strict catalogue record the data plane parses. Every field the
    contract declares is present (nullable where declared), and the id is
    exactly product/profile/version."""
    return {
        "id": f"{args.product}/{args.profile}/{args.version}",
        "product": args.product,
        "version": args.version,
        "sourceSha": args.source_sha,
        "profile": args.profile,
        "visibility": args.visibility,
        "status": "published",
        "publishedAt": args.published_at,
        "withdrawnAt": None,
        "recordedAt": args.recorded_at,
        "productionCandidateId": args.production_candidate_id,
        "artifacts": artifacts,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", help="S3 endpoint URL (R2). Omit with --dry-run.")
    parser.add_argument("--bucket", help="Release bucket name.")
    parser.add_argument("--product", required=True, choices=["cadence", "image"])
    parser.add_argument("--profile", required=True, choices=PROFILES)
    parser.add_argument("--version", required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--visibility", default="private", choices=["public", "private"])
    parser.add_argument("--production-candidate-id", default=None)
    parser.add_argument("--published-at", default=None,
                        help="The release's genuine publication instant (the GitHub Release's "
                             "publishedAt read back after publish for public releases). Omit it "
                             "when the fact is not recorded — publishedAt stays null; a mirror "
                             "run never stamps its own clock time as a publication date.")
    parser.add_argument("--artifact", action="append", default=[],
                        metavar="NAME=PATH[:PLATFORM:CONTENT_TYPE]")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    # ---- validate identity and inputs before any storage work ----
    if not VERSION.fullmatch(args.version) or ".." in args.version or set(args.version) == {"."}:
        fail(f"version {args.version!r} is not a safe catalogue segment")
    if not FULL_SHA.fullmatch(args.source_sha):
        fail(f"--source-sha {args.source_sha!r} is not a full lowercase 40-hex commit")
    if args.published_at is not None and not ISO.fullmatch(args.published_at):
        fail(f"--published-at {args.published_at!r} is not an ISO-8601 instant")
    if args.production_candidate_id is not None and not (
        1 <= len(args.production_candidate_id) <= 200
    ):
        fail("--production-candidate-id is empty or over 200 chars")
    if len(args.artifact) > MAX_ARTIFACTS:
        fail(f"more than {MAX_ARTIFACTS} artifacts")

    artifacts = []
    seen = set()
    for spec in args.artifact:
        if "=" not in spec:
            fail(f"--artifact {spec!r} is not NAME=PATH[:PLATFORM:CONTENT_TYPE]")
        name, rest = spec.split("=", 1)
        parts = rest.rsplit(":", 2)
        if len(parts) == 1:
            path, platform, ctype = parts[0], "any", "application/octet-stream"
        elif len(parts) == 2 and parts[1] in CONTENT_TYPES:
            path, platform, ctype = parts[0], "any", parts[1]
        elif len(parts) == 3 and parts[2] in CONTENT_TYPES:
            path, platform, ctype = parts[0], parts[1], parts[2]
        else:
            fail(f"--artifact {spec!r} is not NAME=PATH[:PLATFORM:CONTENT_TYPE]")
        if not ARTIFACT_NAME.fullmatch(name) or ".." in name:
            fail(f"artifact name {name!r} is not a safe segment")
        if name in seen:
            fail(f"duplicate artifact name {name!r}")
        seen.add(name)
        if not platform or len(platform) > 64:
            fail(f"artifact {name!r} platform is missing or oversized")
        if ctype not in CONTENT_TYPES:
            fail(f"artifact {name!r} content type {ctype!r} is not an allowed type")
        src = Path(path)
        if not src.is_file() or src.is_symlink():
            fail(f"artifact source {path!r} is not a regular file")
        size = src.stat().st_size
        if size == 0 or size > MAX_OBJECT_BYTES:
            fail(f"artifact {name!r} is {size} bytes, outside 1..={MAX_OBJECT_BYTES}")
        artifacts.append(
            {
                "name": name,
                "profile": args.profile,
                "platform": platform,
                "sha256": f"sha256:{sha256_of(src)}",
                "size": size,
                "contentType": ctype,
                "_src": str(src),
            }
        )

    # recordedAt is this run's catalogue write time, always a real
    # instant; publishedAt stays exactly what the caller stamped (null
    # when not supplied — the UI shows Recorded, never an invented date).
    args.recorded_at = _utcnow()
    entry = entry_document(args, [{k: v for k, v in a.items() if k != "_src"} for a in artifacts])
    entry_bytes = json.dumps(entry, separators=(",", ":"), sort_keys=True).encode()

    keys = {
        "catalogue": f"catalogue/{args.product}/{args.profile}/{args.version}.json",
        "objects": [
            f"objects/{args.product}/{args.profile}/{args.version}/{a['name']}"
            for a in artifacts
        ],
    }
    if args.dry_run:
        print(json.dumps({"plan": keys, "entry": entry}, indent=2))
        return

    if not args.endpoint or not args.bucket:
        fail("not_configured: --endpoint and --bucket are required to publish (no storage guessed)")
    env = dict(os.environ)
    if not (
        env.get("AWS_PROFILE")
        or (env.get("AWS_ACCESS_KEY_ID") and env.get("AWS_SECRET_ACCESS_KEY"))
        or env.get("AWS_WEB_IDENTITY_TOKEN_FILE")
    ):
        fail(
            "not_configured: no AWS credentials in the environment — set AWS_PROFILE or "
            "AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY through the operator's provisioned storage "
            "configuration; credentials are never arguments or output"
        )

    # ---- publish objects first, catalogue record last ----
    staged = []
    try:
        for artifact in artifacts:
            key = f"objects/{args.product}/{args.profile}/{args.version}/{artifact['name']}"
            src = artifact["_src"]
            if put_new(args.endpoint, args.bucket, key, src, artifact["contentType"], env):
                staged.append(key)
                continue
            # Already present: identical bytes are an idempotent retry of
            # the same publication; different bytes are a conflict.
            with tempfile.TemporaryDirectory() as td:
                got = Path(td) / "existing"
                download(args.endpoint, args.bucket, key, got, env)
                if sha256_of(got) != sha256_of(src):
                    fail(
                        f"conflict: {key} already exists with different bytes — refusing; "
                        "published artifacts are immutable and never repaired in place"
                    )
                staged.append(key)

        with tempfile.TemporaryDirectory() as td:
            entry_file = Path(td) / "entry.json"
            entry_file.write_bytes(entry_bytes)
            ckey = keys["catalogue"]
            if not put_new(args.endpoint, args.bucket, ckey, entry_file, "application/json", env):
                got = Path(td) / "existing.json"
                download(args.endpoint, args.bucket, ckey, got, env)
                existing = got.read_bytes()
                if existing != entry_bytes:
                    # An identical replay keeps the FIRST recorded
                    # facts: recordedAt differs on every retry, so a
                    # conflict means real divergence — unless the only
                    # difference is this run's fresh recordedAt stamp.
                    # Every other field (identity, visibility, status,
                    # artifact digests) must already match exactly, and
                    # the existing record's recordedAt must be a real
                    # ISO instant; then the replay accepts the original
                    # bytes untouched and never overwrites.
                    if not _identical_replay(existing, entry):
                        fail(
                            f"conflict: catalogue entry {ckey} already exists with different metadata — "
                            "refusing; an approved release record is never overwritten"
                        )
                    entry_bytes = existing

        # ---- reconcile: read back the catalogue and every object head ----
        with tempfile.TemporaryDirectory() as td:
            got = Path(td) / "readback.json"
            download(args.endpoint, args.bucket, keys["catalogue"], got, env)
            if got.read_bytes() != entry_bytes:
                fail("reconcile: the written catalogue entry does not match the recorded bytes")
        for artifact in artifacts:
            key = f"objects/{args.product}/{args.profile}/{args.version}/{artifact['name']}"
            head = head_object(args.endpoint, args.bucket, key, env)
            if head is None or head[1] != artifact["size"]:
                fail(f"reconcile: artifact {key} is missing or the wrong size after write")
    except RuntimeError as e:
        fail(f"held: storage operation failed after {len(staged)} artifact write(s): {e}")

    print(
        json.dumps(
            {
                "published": True,
                "id": entry["id"],
                "catalogue": keys["catalogue"],
                "artifacts": [a["name"] for a in artifacts],
                "visibility": args.visibility,
                "status": "published",
            },
            indent=2,
        )
    )


def _identical_replay(existing_bytes, candidate):
    """True only when the stored entry differs from `candidate` solely
    by its own earlier recordedAt — the same replay of the same
    publication. Every other field must be identical, the stored
    recordedAt must be a real ISO instant, and the stored record must
    already carry status published. Anything else is a real conflict.
    Returns the truthy original recordedAt on match, else False — the
    caller keeps the stored bytes untouched either way (it never
    rewrites the record)."""
    try:
        stored = json.loads(existing_bytes)
    except (ValueError, UnicodeDecodeError):
        return False
    if not isinstance(stored, dict):
        return False
    original = stored.get("recordedAt")
    candidate_stamp = candidate.get("recordedAt")
    if (
        not isinstance(original, str)
        or not ISO.fullmatch(original)
        or not isinstance(candidate_stamp, str)
        or not ISO.fullmatch(candidate_stamp)
    ):
        return False
    # Compare against a candidate stamped with the stored recordedAt:
    # equal then means the replay's only divergence was its fresh
    # write-time stamp.
    stamped = dict(candidate)
    stamped["recordedAt"] = original
    return stored == stamped


def _utcnow():
    import datetime

    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


if __name__ == "__main__":
    main()
