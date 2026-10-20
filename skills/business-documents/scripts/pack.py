#!/usr/bin/env python3
"""Shared template-pack support for the fixed document actions (AOS-310).

A *template pack* is the unit a company supplies once and reuses for every
document of a kind:

    manifest.json      id, revision, format, template, profile
    profile.json       the declarative checks (unchanged CAD-1345 shape)
    rules.md           prose rules the machine cannot check
    sample/            synthetic inputs (never client data)
    <template>         the company's own DOCX or XLSX, untouched

The actions never take a path or a flag from a request: the server stages
each file by role and these helpers receive the staged path. This module
holds the parts every action shares — profile/manifest validation, the
bounded LibreOffice conversion, XLSX recalculation, and the template-part
guard that refuses a workbook whose unsupported parts would be dropped
silently.
"""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import tempfile
import zipfile

PACK_SCHEMA = "business-documents-template-pack@1"
FORMATS = ("docx", "xlsx")

# Package parts openpyxl does not write back. Every other part the template
# carries — under xl/ or anywhere else — must still be present in the output,
# or the helper refuses rather than publish a workbook with a chart, pivot,
# slicer or customXml part missing. Content lost inside a retained part is
# caught by the unsupported-feature warnings escalated in render_sheet.
VOLATILE_XLSX_PARTS = {
    # openpyxl rewrites the formula chain on save; no content is lost.
    "xl/calcChain.xml",
    # Excel-authored workbooks keep text in a shared-string table; openpyxl
    # writes that same text inline, so the member disappears while the strings
    # do not. Rich-text run formatting inside a shared string is flattened —
    # a documented fidelity limit, not a silent part loss.
    "xl/sharedStrings.xml",
}


class PackError(ValueError):
    """A malformed pack or an input the action refuses before any work."""


def read_json(path: Path, label: str) -> dict:
    try:
        value = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PackError(f"{label} is not readable JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise PackError(f"{label} must be a JSON object")
    return value


def load_profile(profile_path: Path) -> dict:
    profile = read_json(profile_path, "profile")
    fmt = profile.get("format")
    if fmt not in FORMATS:
        raise PackError(f"profile format must be docx or xlsx, got {fmt!r}")
    # The hosted action stages the template by role, so a profile's own
    # relative template path is optional there; the local CLI resolves it
    # beside the profile instead.
    return profile


def load_manifest(manifest_path: Path | None, profile: dict) -> dict | None:
    if manifest_path is None:
        return None
    manifest = read_json(manifest_path, "manifest")
    if manifest.get("schema") != PACK_SCHEMA:
        raise PackError(f"manifest schema must be {PACK_SCHEMA}")
    for key in ("id", "revision", "format", "template", "profile"):
        if key not in manifest:
            raise PackError(f"manifest is missing {key!r}")
    if manifest["format"] != profile.get("format"):
        raise PackError("manifest and profile disagree on format")
    revision = manifest["revision"]
    if not isinstance(revision, int) or isinstance(revision, bool) or revision < 1:
        raise PackError("manifest revision must be a positive integer")
    return manifest


def summary_label(manifest: dict | None) -> str:
    label = (manifest or {}).get("summary_label", "output")
    if not isinstance(label, str) or not label.strip():
        return "output"
    return label.strip()[:40]


# An Excel-authored workbook may carry cached formula results. LibreOffice's
# default load mode can reuse them, which would let `check` verify a stale
# value instead of the document's own formulas. This profile entry forces a
# full recalculation on every load; the profile is private to one call.
RECALC_PROFILE = """<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry" xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
 <item oor:path="/org.openoffice.Office.Calc/Formula/Load"><prop oor:name="OOXMLRecalcMode" oor:op="fuse"><value>0</value></prop></item>
 <item oor:path="/org.openoffice.Office.Calc/Formula/Load"><prop oor:name="ODFRecalcMode" oor:op="fuse"><value>0</value></prop></item>
</oor:items>
"""


def libreoffice(args: list[str], timeout: int = 85) -> None:
    """Run one bounded headless LibreOffice conversion."""
    with tempfile.TemporaryDirectory(prefix="cadence-lo-profile-") as profile_dir:
        user = Path(profile_dir) / "profile" / "user"
        user.mkdir(parents=True, exist_ok=True)
        (user / "registrymodifications.xcu").write_text(RECALC_PROFILE, encoding="utf-8")
        command = [
            "soffice",
            "--headless",
            f"-env:UserInstallation=file://{profile_dir}/profile",
            "--norestore",
        ] + args
        try:
            subprocess.run(command, check=True, capture_output=True, text=True, timeout=timeout)
        except FileNotFoundError as exc:
            raise PackError("LibreOffice is unavailable in this converter image") from exc
        except subprocess.TimeoutExpired as exc:
            raise PackError(f"LibreOffice exceeded {timeout}s") from exc
        except subprocess.CalledProcessError as exc:
            detail = (exc.stderr or exc.stdout or "").strip()[:300]
            raise PackError(f"LibreOffice refused the conversion: {detail}") from exc


def convert_pdf(source: Path, output_dir: Path, stem: str) -> Path:
    """Convert one DOCX/XLSX to PDF in a private staging dir and return it."""
    with tempfile.TemporaryDirectory(prefix="cadence-lo-", dir="/tmp") as staging:
        libreoffice(["--convert-to", "pdf", "--outdir", staging, str(source)])
        produced = Path(staging) / f"{source.stem}.pdf"
        if produced.is_symlink() or not produced.is_file() or produced.stat().st_size == 0:
            raise PackError("LibreOffice did not create the expected PDF")
        target = output_dir / f"{stem}.pdf"
        target.write_bytes(produced.read_bytes())
        return target


def recalculate_xlsx(source: Path) -> Path:
    """Return a LibreOffice-recalculated copy used only to read values.

    openpyxl writes formulas without cached results, so the converter's
    LibreOffice recomputes them on load. The copy is private: the published
    workbook stays the byte-faithful openpyxl output.
    """
    with tempfile.TemporaryDirectory(prefix="cadence-lo-", dir="/tmp") as staging:
        libreoffice(["--convert-to", "xlsx", "--outdir", staging, str(source)])
        produced = Path(staging) / f"{source.stem}.xlsx"
        if produced.is_symlink() or not produced.is_file() or produced.stat().st_size == 0:
            raise PackError("LibreOffice did not produce a recalculated workbook")
        # The temp dir dies with this function; hand back an owned copy.
        keep = tempfile.NamedTemporaryFile(
            prefix="cadence-recalc-", suffix=".xlsx", dir="/tmp", delete=False
        )
        keep.close()
        Path(keep.name).write_bytes(produced.read_bytes())
        return Path(keep.name)


def package_parts(path: Path) -> set[str]:
    with zipfile.ZipFile(path) as archive:
        return set(archive.namelist())


def missing_parts(template: Path, output: Path) -> list[str]:
    """Parts present in the template but absent from the output.

    `xl/calcChain.xml` is volatile by design (openpyxl rewrites the chain),
    so it is not a fidelity loss; anything else is refused by the caller.
    """
    before, after = package_parts(template), package_parts(output)
    return sorted(before - after - VOLATILE_XLSX_PARTS)


def parse_ranges(ranges: list[str]) -> list[tuple[int, int, int, int]]:
    """Parse range refs once; cell loops must not re-parse per cell."""
    from openpyxl.utils import range_boundaries

    return [range_boundaries(ref) for ref in ranges]


def inside_bounds(bounds: list[tuple[int, int, int, int]], row: int, col: int) -> bool:
    return any(r1 <= row <= r2 and c1 <= col <= c2 for c1, r1, c2, r2 in bounds)
