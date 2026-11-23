"""Self-test for the website-builder scripts. Exit 0 when every check passes.

Usage: python3 scripts/selftest.py

Runs the state machine (happy path plus three refusals), a reproducibility
check on build.py, and check.py against a local http.server fixture. Uses only
a temporary directory and 127.0.0.1.
"""

import json
import os
import re
import subprocess
import sys
import tempfile
import threading
from functools import partial
from http.server import HTTPServer, SimpleHTTPRequestHandler

HERE = os.path.dirname(os.path.abspath(__file__))
PAGE = b"<!doctype html><html><head><meta charset=utf-8><title>t</title></head><body>hi</body></html>\n"


def script(name, *args):
    result = subprocess.run(
        [sys.executable, os.path.join(HERE, name), *args],
        capture_output=True,
        text=True,
        timeout=60,
    )
    return result


def parse(result):
    return json.loads(result.stdout)


def expect(label, condition):
    if not condition:
        raise AssertionError(label)
    print(f"ok - {label}")


def is_digest_name(name):
    stem, ext = os.path.splitext(os.path.basename(name))
    return len(stem) == 64 and all(c in "0123456789abcdef" for c in stem) and ext in (".css", ".png", ".html")


def write_json(path, value):
    with open(path, "w", encoding="utf-8") as handle:
        json.dump(value, handle)


def state_machine(root):
    run = os.path.join(root, "run")
    first = parse(script("next.py", "--run", run))
    expect("next starts at brief", first["step"] == "brief" and first["status"] == "awaiting-owner")

    data = os.path.join(root, "data.json")
    write_json(data, {"step": "content", "data": {"src": "site", "facts_from_owner": True}})
    early = script("submit.py", "--run", run, "--data", data)
    expect("refuses out-of-order submit", early.returncode == 2 and "out of order" in early.stderr)

    write_json(data, {"step": "bogus", "data": {}})
    unknown = script("submit.py", "--run", run, "--data", data)
    expect("refuses unknown step", unknown.returncode == 2 and "unknown step" in unknown.stderr)

    with open(os.path.join(run, "run.json"), "w", encoding="utf-8") as handle:
        handle.write("{not json")
    malformed = script("submit.py", "--run", run, "--data", data)
    expect("refuses malformed state", malformed.returncode == 2 and "malformed" in malformed.stderr)

    brief = {"step": "brief", "data": {"owner_confirmed": True, "goal": "portfolio", "name": "demo-site"}}
    os.remove(os.path.join(run, "run.json"))
    parse(script("next.py", "--run", run))
    write_json(data, brief)
    accepted = parse(script("submit.py", "--run", run, "--data", data))
    expect("brief advances to content", accepted["next"]["step"] == "content")
    again = parse(script("submit.py", "--run", run, "--data", data))
    expect("resubmit same brief is idempotent", again["idempotent"] is True and again["next"]["step"] == "content")

    write_json(data, {"step": "brief", "data": {"owner_confirmed": True, "goal": "changed", "name": "demo-site"}})
    changed = script("submit.py", "--run", run, "--data", data)
    expect("refuses changed brief after the step", changed.returncode == 2)
    return run


def build_and_check(root, run):
    src = os.path.join(root, "site")
    os.makedirs(os.path.join(src, "css"))
    with open(os.path.join(src, "index.html"), "wb") as handle:
        handle.write(b'<!doctype html><html><head><link rel="stylesheet" href="css/site.css"></head><body><a href="pages/about.html">About</a><img src="css/logo.png"></body></html>\n')
    os.makedirs(os.path.join(src, "pages"))
    with open(os.path.join(src, "pages", "about.html"), "wb") as handle:
        handle.write(b'<!doctype html><html><body><img src="../logo.png"></body></html>\n')
    with open(os.path.join(src, "logo.png"), "wb") as handle:
        handle.write(b"\x89PNG\r\n\x1a\nroot")
    with open(os.path.join(src, "css", "site.css"), "wb") as handle:
        handle.write(b'body{margin:0;background:url("logo.png")} .label::after{content:"url(logo.png)"}\n')
    with open(os.path.join(src, "css", "logo.png"), "wb") as handle:
        handle.write(b"\x89PNG\r\n\x1a\nfake")

    out_a = os.path.join(root, "dist-a")
    out_b = os.path.join(root, "dist-b")
    first = parse(script("build.py", "--run", run, "--src", src, "--out", out_a))
    second = parse(script("build.py", "--run", run, "--src", src, "--out", out_b))
    expect("build reports files and bytes", first["files"] == 5 and first["bytes"] > 0)
    expect("second build reports the same totals", (first["files"], first["bytes"]) == (second["files"], second["bytes"]))
    tree_a = sorted(os.path.relpath(os.path.join(d, f), out_a) for d, _, fs in os.walk(out_a) for f in fs)
    tree_b = sorted(os.path.relpath(os.path.join(d, f), out_b) for d, _, fs in os.walk(out_b) for f in fs)
    expect("build output names are digest-first", all(
        name == "index.html" or is_digest_name(name) for name in tree_a
    ))
    for name in tree_a:
        with open(os.path.join(out_a, name), "rb") as a, open(os.path.join(out_b, name), "rb") as b:
            expect(f"reproducible bytes {name}", a.read() == b.read())
    expect("same file set on rerun", tree_a == tree_b)
    with open(os.path.join(out_a, "index.html"), "rb") as handle:
        html = handle.read().decode()
    expect("html rewritten to digest names", "css/site.css" not in html and "css/logo.png" not in html)
    built_css = [name for name in os.listdir(os.path.join(out_a, "css")) if name.endswith(".css")][0]
    with open(os.path.join(out_a, "css", built_css), "rb") as handle:
        css = handle.read().decode()
    built_png = [name for name in os.listdir(os.path.join(out_a, "css")) if name.endswith(".png")][0]
    expect("css url points to the built image", f'url("{built_png}")' in css)
    expect("css string content is unchanged", 'content:"url(logo.png)"' in css)
    nested = [
        os.path.join(d, f)
        for d, _, files in os.walk(os.path.join(out_a, "pages"))
        for f in files
        if f.endswith(".html")
    ]
    expect("nested page is built", len(nested) == 1)
    with open(nested[0], "rb") as handle:
        nested_html = handle.read().decode()
    found = re.search(r'src="([^"]+)"', nested_html)
    expect("nested page image is rewritten", found is not None and "../logo.png" not in nested_html)
    resolved = os.path.normpath(os.path.join(os.path.dirname(nested[0]), found.group(1)))
    expect("nested page image resolves inside dist", os.path.isfile(resolved))
    with open(os.path.join(out_a, "index.html"), "rb") as handle:
        root_html = handle.read().decode()
    link = re.search(r'href="([^"]+)"', root_html)
    expect("root page link resolves inside dist", link is not None and os.path.isfile(
        os.path.normpath(os.path.join(out_a, link.group(1)))))

    return out_a


class FormHandler(SimpleHTTPRequestHandler):
    """Serves files; answers POST with the configured form status."""

    form_status = 405
    posted = []

    def do_POST(self):
        length = int(self.headers.get("content-length") or 0)
        FormHandler.posted.append(self.rfile.read(length))
        self.send_response(FormHandler.form_status)
        self.send_header("content-length", "0")
        self.end_headers()

    def end_headers(self):
        self.send_header("content-security-policy", "default-src 'self'")
        self.send_header("x-content-type-options", "nosniff")
        super().end_headers()


def check_fixture(root, dist):
    FormHandler.form_status = 405
    FormHandler.posted = []
    handler = partial(FormHandler, directory=dist)
    server = HTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        url = f"http://127.0.0.1:{server.server_address[1]}"
        run = os.path.join(root, "run")
        result = script("check.py", "--run", run, "--url", url, "--dist", dist)
        body = parse(result)
        expect("check reports 200 and matching bytes", body["status"] == 200 and body["bytes_match"] is True)
        expect("check maps 405 form probe to not_available", body["form"] == "not_available")
        probe = FormHandler.posted[-1] if FormHandler.posted else b""
        expect("form probe carries the honeypot", b'"_hp"' in probe)
        expect("check reads the CSP header", body["csp"] == "default-src 'self'")
        expect("check reads nosniff", body["nosniff"] is True)
        expect("check exits 0 when served and matching", result.returncode == 0)
        mismatch = os.path.join(root, "mismatch")
        os.makedirs(mismatch)
        with open(os.path.join(mismatch, "index.html"), "wb") as handle:
            handle.write(PAGE)
        off = script("check.py", "--run", run, "--url", url, "--dist", mismatch)
        expect("check exits 1 when bytes differ", off.returncode == 1 and parse(off)["bytes_match"] is False)
        bad = script("check.py", "--run", run, "--url", "http://127.0.0.1:1", "--dist", dist)
        expect("check exits 1 when unreachable", bad.returncode == 1)
        FormHandler.form_status = 201
        live = script("check.py", "--run", run, "--url", url, "--dist", dist)
        expect("check maps 201 form probe to available", parse(live)["form"] == "available")
        expect("check accepts an explicit form key", script(
            "check.py", "--run", run, "--url", url, "--dist", dist, "--form", "contact").returncode == 0)
        done_flow(root, run, url, dist)
    finally:
        server.shutdown()
        server.server_close()


def submit_step(root, run, step, data):
    payload = os.path.join(root, f"{step}.json")
    write_json(payload, {"step": step, "data": data})
    return script("submit.py", "--run", run, "--data", payload)


def done_flow(root, run, url, dist):
    """Reach done through the remaining steps, then damage the recorded verify."""

    with open(os.path.join(run, "artifacts", "build-manifest.json"), encoding="utf-8") as handle:
        manifest = json.load(handle)
    for step, data in (
        ("content", {"src": "site", "facts_from_owner": True}),
        ("build", {"out": manifest["out"], "files": manifest["files"], "bytes": manifest["bytes"]}),
        ("review", {"owner_approved": True}),
        ("publish", {"owner_approved": True, "release": "rel-fixture", "slug": "demo-site"}),
    ):
        expect(f"{step} submits", submit_step(root, run, step, data).returncode == 0)
    expect("verify refuses owner acceptance before a recorded check", submit_step(
        root, run, "verify", {"owner_accepted": True}).returncode == 2)
    recorded = script("check.py", "--run", run, "--url", url, "--dist", dist, "--record")
    expect("check --record writes the evidence", recorded.returncode == 0)
    expect("verify submits after the recorded check", submit_step(
        root, run, "verify", {"owner_accepted": True}).returncode == 0)
    finished = script("next.py", "--run", run)
    expect("a full run reaches done", finished.returncode == 0 and parse(finished)["step"] == "done")
    with open(os.path.join(run, "run.json"), encoding="utf-8") as handle:
        state = json.load(handle)
    state["verify"].pop("url")
    write_json(os.path.join(run, "run.json"), state)
    damaged = script("next.py", "--run", run)
    expect("refuses a completed verify without its checked URL", damaged.returncode == 2)
    expect("damaged verify refusal is one line", len(damaged.stderr.strip().splitlines()) == 1)


def resume_validation(root):
    run = os.path.join(root, "bad-run")
    parse(script("next.py", "--run", run))
    expect("brief submits before the resume check", submit_step(
        root, run, "brief",
        {"owner_confirmed": True, "goal": "Ship the site", "name": "demo-site"}).returncode == 0)
    path = os.path.join(run, "run.json")
    with open(path, encoding="utf-8") as handle:
        state = json.load(handle)
    state["brief"] = {}
    write_json(path, state)
    resumed = script("next.py", "--run", run)
    expect("refuses a malformed completed checkpoint", resumed.returncode == 2)
    expect("malformed checkpoint refusal is one line", len(resumed.stderr.strip().splitlines()) == 1)


def main():
    with tempfile.TemporaryDirectory(prefix="website-builder-selftest-") as root:
        run = state_machine(root)
        dist = build_and_check(root, run)
        check_fixture(root, dist)
        resume_validation(root)
    print("selftest: all checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
