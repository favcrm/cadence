"""Freeze a source folder into dist/ with digest-first asset names.

Usage: python3 scripts/build.py --run DIR --src SRC --out OUT

Each HTML, CSS, JS, SVG and PNG file is copied as <sha256 of its bytes>.<ext>
in the same directory. index.html stays index.html. References in HTML
(href, src, content) that point at a copied file are rewritten. Two runs over
the same source yield byte-identical output. Writes only under OUT and
DIR/artifacts/.
"""

import argparse
import hashlib
import json
import os
import posixpath
import re
from urllib.parse import urlsplit

from common import MANIFEST, ORDER, load, refuse, run, emit

EXTENSIONS = {
    ".html", ".css", ".js", ".mjs", ".svg", ".png",
    ".jpg", ".jpeg", ".webp", ".avif", ".ico",
    ".woff", ".woff2", ".ttf", ".otf",
}
ATTR = re.compile(r"""\b(href|src|content)\s*=\s*(["'])(.*?)\2""", re.IGNORECASE | re.DOTALL)
CSS_URL = re.compile(r"""url\(\s*(["']?)([^"')]+)\1\s*\)""")


def sources(src):
    found = []
    for root, dirs, names in os.walk(src):
        dirs[:] = sorted(d for d in dirs if not d.startswith("."))
        for name in sorted(names):
            if name.startswith(".") or os.path.islink(os.path.join(root, name)):
                continue
            rel = os.path.relpath(os.path.join(root, name), src).replace(os.sep, "/")
            if os.path.splitext(name)[1].lower() in EXTENSIONS:
                found.append(rel)
    return found


def mapping(src, rels):
    table = {}
    for rel in rels:
        if rel == "index.html":
            table[rel] = rel
            continue
        with open(os.path.join(src, rel), "rb") as handle:
            digest = hashlib.sha256(handle.read()).hexdigest()
        ext = os.path.splitext(rel)[1].lower()
        head = posixpath.dirname(rel)
        table[rel] = posixpath.join(head, f"{digest}{ext}") if head else f"{digest}{ext}"
    return table


def rewrite(text, table):
    def swap(match):
        attr, quote, value = match.group(1), match.group(2), match.group(3)
        if not value or value.startswith(("#", "//", "data:", "mailto:", "tel:")):
            return match.group(0)
        parts = urlsplit(value)
        if parts.scheme or parts.netloc:
            return match.group(0)
        absolute = parts.path.startswith("/")
        rel = posixpath.normpath(parts.path.lstrip("/"))
        if rel.startswith(".."):
            return match.group(0)
        if rel not in table:
            return match.group(0)
        new = table[rel]
        if absolute:
            new = "/" + new
        suffix = ("?" + parts.query if parts.query else "") + (
            "#" + parts.fragment if parts.fragment else ""
        )
        return f'{attr}={quote}{new}{suffix}{quote}'

    return ATTR.sub(swap, text)


def rewrite_css(text, table, stylesheet):
    """Rewrite url(...) references in a stylesheet to the digest names."""

    def swap(match):
        quote, value = match.group(1), match.group(2).strip()
        if not value or value.startswith(("#", "//", "data:", "mailto:", "tel:")):
            return match.group(0)
        parts = urlsplit(value)
        if parts.scheme or parts.netloc:
            return match.group(0)
        absolute = parts.path.startswith("/")
        directory = posixpath.dirname(stylesheet) or "."
        rel = posixpath.normpath(
            parts.path.lstrip("/") if absolute else posixpath.join(directory, parts.path)
        )
        if rel.startswith("..") or rel not in table:
            return match.group(0)
        new = "/" + table[rel] if absolute else posixpath.relpath(table[rel], directory)
        suffix = ("?" + parts.query if parts.query else "") + (
            "#" + parts.fragment if parts.fragment else ""
        )
        return f"url({quote}{new}{suffix}{quote})"

    result = []
    i = 0
    while i < len(text):
        if text.startswith("/*", i):
            end = text.find("*/", i + 2)
            end = len(text) if end < 0 else end + 2
        elif text[i] in "\"'":
            quote = text[i]
            end = i + 1
            while end < len(text):
                if text[end] == "\\":
                    end += 2
                elif text[end] == quote:
                    end += 1
                    break
                else:
                    end += 1
            end = min(end, len(text))
        elif text[i] == "\\":
            end = min(i + 2, len(text))
        else:
            match = CSS_URL.match(text, i)
            if match and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] in "_-")):
                result.append(swap(match))
                i = match.end()
                continue
            end = i + 1
        result.append(text[i:end])
        i = end
    return "".join(result)


def remove_previous(out, names):
    """Delete only files a previous build of this same OUT wrote."""
    for name in names:
        path = os.path.join(out, name)
        if os.path.isfile(path):
            os.remove(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--run", required=True, metavar="DIR")
    parser.add_argument("--src", required=True, metavar="SRC")
    parser.add_argument("--out", required=True, metavar="OUT")
    args = parser.parse_args()

    state = load(args.run)
    if ORDER.index(state["step"]) > ORDER.index("build"):
        refuse("the build was already submitted; start a new run to change the site")
    if not os.path.isdir(args.src):
        refuse(f"source folder {args.src} does not exist")
    if not os.path.isfile(os.path.join(args.src, "index.html")):
        refuse(f"source folder {args.src} has no index.html")

    out = os.path.abspath(args.out)
    manifest_path = os.path.join(args.run, MANIFEST)
    previous = {}
    if os.path.isfile(manifest_path):
        with open(manifest_path, encoding="utf-8") as handle:
            prior = json.load(handle)
        if prior.get("out") == out:
            previous = prior.get("map", {})
    rels = sources(args.src)
    table = mapping(args.src, rels)
    # A stylesheet's own name depends on its rewritten content, so hash CSS
    # after its image references point at their digest names.
    styles = {}
    for rel in rels:
        if os.path.splitext(rel)[1].lower() != ".css":
            continue
        with open(os.path.join(args.src, rel), "rb") as handle:
            rewritten = rewrite_css(handle.read().decode("utf-8"), table, rel).encode("utf-8")
        styles[rel] = rewritten
        digest = hashlib.sha256(rewritten).hexdigest()
        head = posixpath.dirname(rel)
        table[rel] = posixpath.join(head, f"{digest}.css") if head else f"{digest}.css"
    remove_previous(out, [name for rel, name in previous.items() if table.get(rel) != name])

    written = {}
    for rel in rels:
        if rel in styles:
            data = styles[rel]
        else:
            with open(os.path.join(args.src, rel), "rb") as handle:
                data = handle.read()
        if os.path.splitext(rel)[1].lower() == ".html":
            text = data.decode("utf-8")
            data = rewrite(text, table).encode("utf-8")
        target = os.path.join(out, table[rel])
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "wb") as handle:
            handle.write(data)
        written[rel] = {"to": table[rel], "bytes": len(data)}

    total = sum(entry["bytes"] for entry in written.values())
    manifest = {
        "out": out,
        "files": len(written),
        "bytes": total,
        "map": {rel: entry["to"] for rel, entry in written.items()},
    }
    os.makedirs(os.path.dirname(manifest_path), exist_ok=True)
    with open(manifest_path, "w", encoding="utf-8") as handle:
        json.dump(manifest, handle, indent=2, sort_keys=True)
        handle.write("\n")
    emit({"out": out, "files": len(written), "bytes": total})
    return 0


if __name__ == "__main__":
    raise SystemExit(run(main))
