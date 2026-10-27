"""Run-state rules shared by the website-builder scripts (stdlib only).

Every command prints one JSON object on stdout. A refusal prints one line on
stderr and exits 2. Nothing here calls cadence, opens a socket, or writes
outside the run directory.
"""

import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone

VERSION = 1
STEPS = ("brief", "content", "build", "review", "publish", "verify")
DONE = "done"
ORDER = STEPS + (DONE,)
SLUG = re.compile(r"[a-z0-9-]{3,63}")
MANIFEST = os.path.join("artifacts", "build-manifest.json")

STEP_INFO = {
    "brief": {
        "status": "awaiting-owner",
        "question": "Owner: what is the site for, who reads it, which pages does it need, "
        "does it need a contact form, and which slug should the address use "
        "(3-63 lowercase letters, digits or hyphens)?",
        "default": {"pages": ["index.html"], "contact_form": False},
        "artifact": "run.json#brief",
        "done_when": "owner confirmed the brief; submit brief with owner_confirmed true, "
        "name (slug) and goal",
    },
    "content": {
        "status": "ready",
        "question": "Gather the owner's own text and images into one source folder that "
        "holds index.html. Use only facts the owner supplied; ask for any missing fact "
        "instead of inventing it.",
        "default": {"src": "site"},
        "artifact": "run.json#content",
        "done_when": "source folder holds index.html and every claim traces to owner "
        "material; submit content with src and facts_from_owner true",
    },
    "build": {
        "status": "ready",
        "question": "Run build.py to freeze the source into dist/ with digest-first asset names.",
        "default": {"out": "dist"},
        "artifact": "dist/index.html",
        "done_when": "build.py printed out, files and bytes; submit build with those values",
    },
    "review": {
        "status": "awaiting-owner",
        "question": "Owner: open the built dist/index.html locally and approve it, or say "
        "what to change.",
        "default": {"owner_approved": False},
        "artifact": "dist/index.html",
        "done_when": "owner approved this exact build; submit review with owner_approved true",
    },
    "publish": {
        "status": "awaiting-owner",
        "question": "Owner: approve the publication link in a browser signed in to the "
        "selected workspace.",
        "default": {"owner_approved": False},
        "artifact": "release id from cadence web prepare",
        "done_when": "owner approved the release in the browser; submit publish with "
        "release, slug and owner_approved true",
    },
    "verify": {
        "status": "awaiting-owner",
        "question": "Owner: the agent has checked the live URL. Does the live site show the "
        "approved page?",
        "default": {"owner_accepted": False},
        "artifact": "run.json#verify",
        "done_when": "check.py --record passed (status 200, bytes_match true) and the owner "
        "accepted; submit verify with owner_accepted true",
    },
}


class Refusal(Exception):
    """A refused command. The message is printed on one stderr line."""


def refuse(message):
    raise Refusal(message)


def run(main):
    """Run main(); map a refusal or I/O failure to one stderr line and exit 2."""
    try:
        return main()
    except (Refusal, OSError) as error:
        sys.stderr.write(f"{os.path.basename(sys.argv[0])}: refused: {error}\n")
        return 2


def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def digest(value):
    """sha256 of the canonical JSON form of value."""
    text = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def emit(value):
    sys.stdout.write(json.dumps(value, sort_keys=True, ensure_ascii=False) + "\n")


def state_path(run_dir):
    return os.path.join(run_dir, "run.json")


def new_state(run_dir, stamp):
    state = {
        "version": VERSION,
        "id": os.path.basename(os.path.abspath(run_dir)),
        "created_at": stamp,
        "updated_at": stamp,
        "step": STEPS[0],
        "status": "open",
        "history": [],
    }
    for step in STEPS:
        state[step] = None
    return state


def validate(state):
    if not isinstance(state, dict):
        refuse("malformed run.json: top level is not an object")
    keys = ["version", "id", "created_at", "updated_at", "step", "status", "history", *STEPS]
    missing = [key for key in keys if key not in state]
    if missing:
        refuse(f"malformed run.json: missing {', '.join(missing)}")
    if state["version"] != VERSION:
        refuse(f"malformed run.json: version {state['version']!r}, expected {VERSION}")
    if not isinstance(state["id"], str) or not state["id"]:
        refuse("malformed run.json: id must be a non-empty string")
    if state["step"] not in ORDER:
        refuse(f"malformed run.json: unknown step {state['step']!r}")
    if state["status"] not in ("open", DONE):
        refuse(f"malformed run.json: unknown status {state['status']!r}")
    if (state["status"] == DONE) != (state["step"] == DONE):
        refuse("malformed run.json: status and step disagree")
    if not isinstance(state["history"], list):
        refuse("malformed run.json: history must be a list")
    current = ORDER.index(state["step"])
    for position, step in enumerate(STEPS):
        value = state[step]
        if value is not None and not isinstance(value, dict):
            refuse(f"malformed run.json: {step} must be an object or null")
        if position < current and value is None:
            refuse(f"malformed run.json: {step} is missing although the run is past it")
        if position < current and value is not None:
            problem = completed_record_problem(step, value)
            if problem:
                refuse(f"malformed run.json: {step} {problem}")
        if position > current and value is not None:
            refuse(f"malformed run.json: {step} is recorded before its step")


def completed_record_problem(step, data):
    """Why a completed step's record is incomplete, or None when it is sound.

    Resume reads the file, so a record the run has already passed must still
    carry the fields its own step requires. Filesystem evidence (the build
    manifest) is not re-read here: it may legitimately be gone.
    """

    def yes(key):
        return None if data.get(key) is True else f"needs {key} true"

    def text(key):
        value = data.get(key)
        if not isinstance(value, str) or not value.strip():
            return f"needs a non-empty {key}"
        return None

    def slug(key):
        value = data.get(key)
        if not isinstance(value, str) or not SLUG.fullmatch(value):
            return f"needs {key} to be a slug"
        return None

    def count(key):
        value = data.get(key)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            return f"needs a non-negative integer {key}"
        return None

    if step == "brief":
        return yes("owner_confirmed") or text("goal") or slug("name")
    if step == "content":
        return text("src") or yes("facts_from_owner")
    if step == "build":
        return text("out") or count("files") or count("bytes")
    if step == "review":
        return yes("owner_approved")
    if step == "publish":
        return yes("owner_approved") or text("release") or slug("slug")
    if step == "verify":
        if data.get("status") != 200 or data.get("bytes_match") is not True:
            return "needs the recorded passing check (status 200, bytes_match true)"
        problem = yes("owner_accepted") or text("url") or text("checked_at")
        if problem:
            return problem
        if "form" not in data:
            return "needs the recorded form result"
        return None
    return None


def load(run_dir):
    if not os.path.isfile(state_path(run_dir)):
        refuse(f"no run.json in {run_dir}; start the run with next.py --run {run_dir}")
    try:
        with open(state_path(run_dir), encoding="utf-8") as handle:
            state = json.load(handle)
    except ValueError as error:
        refuse(f"malformed run.json: {error}")
    validate(state)
    return state


def save(run_dir, state, stamp):
    state["updated_at"] = stamp
    temp = os.path.join(run_dir, ".run.json.tmp")
    with open(temp, "w", encoding="utf-8") as handle:
        json.dump(state, handle, indent=2, sort_keys=True, ensure_ascii=False)
        handle.write("\n")
    os.replace(temp, state_path(run_dir))


def view(state):
    step = state["step"]
    if step == DONE:
        return {
            "step": DONE,
            "status": DONE,
            "question": None,
            "default": None,
            "artifact": "run.json",
            "done_when": "the run reached done; every step and the live check are in run.json",
        }
    info = STEP_INFO[step]
    return {
        "step": step,
        "status": info["status"],
        "question": info["question"],
        "default": info["default"],
        "artifact": info["artifact"],
        "done_when": info["done_when"],
    }


def read_manifest(run_dir):
    path = os.path.join(run_dir, MANIFEST)
    if not os.path.isfile(path):
        return None
    try:
        with open(path, encoding="utf-8") as handle:
            return json.load(handle)
    except ValueError as error:
        refuse(f"malformed {MANIFEST}: {error}")


def check_record(step, data, run_dir, state):
    """Refuse a submitted step record that is incomplete or not backed by evidence.

    Evidence for verify comes only from run.verify, which check.py --record
    writes; owner-submitted data cannot assert a passing check.
    """

    def fail(message):
        refuse(f"{step}: {message}")

    def owner_yes(key):
        if data.get(key) is not True:
            fail(f"{key} must be true, recorded from the owner's own answer")

    def text(key):
        if not isinstance(data.get(key), str) or not data[key].strip():
            fail(f"{key} must be a non-empty string")

    def slug(key):
        if not isinstance(data.get(key), str) or not SLUG.fullmatch(data[key]):
            fail(f"{key} must be a slug: 3-63 lowercase letters, digits or hyphens")

    if step == "brief":
        owner_yes("owner_confirmed")
        text("goal")
        slug("name")
    elif step == "content":
        text("src")
        owner_yes("facts_from_owner")
    elif step == "build":
        text("out")
        for key in ("files", "bytes"):
            value = data.get(key)
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                fail(f"{key} must be a non-negative integer")
        manifest = read_manifest(run_dir)
        if manifest is None:
            fail("build.py has not written a build for this run")
        if os.path.abspath(data["out"]) != manifest["out"]:
            fail(f"out {data['out']!r} is not the folder build.py wrote")
        if data["files"] != manifest["files"] or data["bytes"] != manifest["bytes"]:
            fail("files and bytes do not match the last build.py run; run build.py again")
    elif step == "review":
        owner_yes("owner_approved")
    elif step == "publish":
        owner_yes("owner_approved")
        text("release")
        slug("slug")
    elif step == "verify":
        owner_yes("owner_accepted")
        evidence = state.get("verify") or {}
        if evidence.get("status") != 200 or evidence.get("bytes_match") is not True:
            fail("check.py --record has not passed for this run; run it first")
