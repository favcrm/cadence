"""Check a served site against dist/index.html and record the evidence.

Usage: python3 scripts/check.py --run DIR --url URL --dist DIR [--record]

GETs URL's "/" and compares the body with DIR/index.html byte for byte. Reads
content-security-policy and x-content-type-options. POSTs an empty JSON object
to /__forms/contact with a same-origin Origin header: a probe that does not
create a real submission. 405 maps to form "not_available"; any other status
is recorded as the number. Exit 0 when the site is served (200) and the bytes
match; exit 1 otherwise. --record writes the result into run.verify and is only
accepted while the run is at the verify step. Nothing else is written.
"""

import argparse
import os
import urllib.error
import urllib.parse
import urllib.request

from common import emit, load, now, refuse, run, save


def fetch(request):
    """Return (status, headers, body). An unreachable host yields status None."""
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return response.status, response.headers, response.read()
    except urllib.error.HTTPError as error:
        return error.code, error.headers, error.read()
    except (urllib.error.URLError, OSError):
        return None, {}, b""


def origin_of(url):
    parts = urllib.parse.urlsplit(url)
    return f"{parts.scheme}://{parts.netloc}"


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--run", required=True, metavar="DIR")
    parser.add_argument("--url", required=True)
    parser.add_argument("--dist", required=True, metavar="DIR")
    parser.add_argument("--record", action="store_true")
    args = parser.parse_args()

    parts = urllib.parse.urlsplit(args.url)
    if parts.scheme not in ("http", "https") or not parts.netloc:
        refuse(f"--url must be an absolute http(s) URL, got {args.url!r}")
    base = origin_of(args.url)
    state = None
    if args.record:
        state = load(args.run)
        if state["step"] != "verify":
            refuse(f"--record is only allowed at the verify step; the run is at {state['step']}")

    expected_path = os.path.join(args.dist, "index.html")
    if not os.path.isfile(expected_path):
        refuse(f"{expected_path} does not exist; run build.py first")
    with open(expected_path, "rb") as handle:
        expected = handle.read()

    status, headers, body = fetch(urllib.request.Request(base + "/"))
    bytes_match = status == 200 and body == expected
    csp = headers.get("content-security-policy")
    nosniff = (headers.get("x-content-type-options") or "").strip().lower() == "nosniff"

    probe = urllib.request.Request(
        base + "/__forms/contact",
        data=b"{}",
        method="POST",
        headers={"content-type": "application/json", "origin": base},
    )
    form_status, _, _ = fetch(probe)
    if form_status is None:
        form = None
    else:
        form = "not_available" if form_status == 405 else form_status

    result = {
        "url": args.url,
        "status": status,
        "bytes_match": bytes_match,
        "csp": csp,
        "nosniff": nosniff,
        "form": form,
        "checked_at": now(),
    }
    if args.record:
        state["verify"] = {**(state["verify"] or {}), **result}
        save(args.run, state, result["checked_at"])
    emit(result)
    return 0 if status == 200 and bytes_match else 1


if __name__ == "__main__":
    raise SystemExit(run(main))
