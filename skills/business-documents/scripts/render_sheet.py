#!/usr/bin/env python3
"""Fill one company XLSX template from a validated context (AOS-310).

The template is the company's own workbook: labels, formulas, validations,
styles, print areas and page setup stay exactly as supplied. This helper
writes **only** the cells the profile declares as inputs:

    input_ranges   {sheet: ["B4:B9", ...]}   scalar cells
    row_tables     [{sheet, rows: [first, last], key, ...}]   row bands

Anything outside those ranges is a refusal, never a silent write. After
saving, the helper compares the workbook's internal parts with the
template's and refuses if openpyxl would drop one (a pivot table, slicer
or macro part), so a template this converter cannot reproduce faithfully
fails loudly instead of publishing a lossy book.

A LibreOffice PDF is produced beside the workbook for the visual check.
Formulas are not persisted with cached values: Excel/LibreOffice compute
them on open, and `check.py` reads a recalculated private copy. Exit 0 =
rendered, 2 = refused before publication.

Usage:
  render_sheet.py --template T.xlsx --profile P.json --context C.json
                  --output-dir DIR [--manifest M.json] [--sample]
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys
import tempfile
import warnings

import pack
from render import exclusive_write, publish_staged_file, safe_output

SCHEMA = "business-documents-render-sheet@1"
# openpyxl says so itself when it is about to drop an OOXML extension it does
# not model. Refusing is the only honest outcome for a company template.
UNSUPPORTED = re.compile(r"not supported|unsupported|will be removed", re.IGNORECASE)
CELL_REF = re.compile(r"^([A-Za-z][A-Za-z0-9 _-]{0,60})!([A-Z]{1,3})([1-9][0-9]{0,6})$")
COLUMN = re.compile(r"^[A-Z]{1,3}$")


def row_band(profile: dict, sheet: str) -> dict | None:
    specs = [spec for spec in profile.get("row_tables", []) if spec.get("sheet") == sheet]
    if len(specs) > 1:
        raise pack.PackError(
            f"the profile declares more than one row_table for {sheet!r}; the context format supports one"
        )
    if not specs:
        return None
    rows = specs[0].get("rows")
    if (
        not isinstance(rows, list)
        or len(rows) != 2
        or not all(isinstance(v, int) and not isinstance(v, bool) and v >= 1 for v in rows)
        or rows[0] > rows[1]
    ):
        raise pack.PackError(f"row_table rows for {sheet!r} must be [first, last] positive integers")
    return specs[0]


def input_ranges(profile: dict, sheet: str) -> list[str]:
    ranges = profile.get("input_ranges", {}).get(sheet, [])
    if not isinstance(ranges, list) or not all(isinstance(r, str) for r in ranges):
        raise pack.PackError(f"profile input_ranges for {sheet!r} must be a list of ranges")
    return ranges


def scalar(value, ref: str):
    if isinstance(value, bool) or isinstance(value, (int, float)):
        return value
    if isinstance(value, str) and value.startswith("="):
        raise pack.PackError(
            f"cell {ref} starts with '='; context values are data, not formulas"
        )
    if isinstance(value, str):
        return value
    if value is None:
        raise pack.PackError(f"cell {ref} has a null value; omit the cell instead")
    raise pack.PackError(f"cell {ref} must be a string, number or boolean")


def apply_cells(wb, profile: dict, cells: dict) -> int:
    from openpyxl.utils import column_index_from_string

    bounds: dict[str, list[tuple[int, int, int, int]]] = {}
    written = 0
    for ref, value in cells.items():
        match = CELL_REF.fullmatch(ref)
        if not match:
            raise pack.PackError(f"cell reference {ref!r} must look like Sheet1!B4")
        sheet, column, row = match.group(1), match.group(2), int(match.group(3))
        if sheet not in wb.sheetnames:
            raise pack.PackError(f"cell {ref} names a sheet the template does not have")
        sheet_bounds = bounds.setdefault(sheet, pack.parse_ranges(input_ranges(profile, sheet)))
        if not pack.inside_bounds(sheet_bounds, row, column_index_from_string(column)):
            raise pack.PackError(f"cell {ref} is outside the template's declared input_ranges")
        wb[sheet][f"{column}{row}"] = scalar(value, ref)
        written += 1
    return written


def apply_tables(wb, profile: dict, tables: list) -> int:
    written = 0
    seen = set()
    for table in tables:
        if not isinstance(table, dict) or set(table) != {"sheet", "rows"}:
            raise pack.PackError("each table entry is {sheet, rows}")
        sheet, rows = table["sheet"], table["rows"]
        if not isinstance(sheet, str) or sheet not in wb.sheetnames:
            raise pack.PackError(f"table names a sheet the template does not have: {sheet!r}")
        if sheet in seen:
            raise pack.PackError(f"table for {sheet!r} appears twice")
        seen.add(sheet)
        spec = row_band(profile, sheet)
        if spec is None:
            raise pack.PackError(f"the profile declares no row_table for {sheet!r}")
        first, last = spec["rows"]
        if not isinstance(rows, list) or not rows:
            raise pack.PackError(f"table for {sheet!r} needs a non-empty rows list")
        if len(rows) > last - first + 1:
            raise pack.PackError(
                f"table for {sheet!r} has {len(rows)} rows; the template declares {last - first + 1}"
            )
        for offset, row in enumerate(rows):
            if not isinstance(row, dict) or not row:
                raise pack.PackError(f"table row {offset + 1} in {sheet!r} must be a column map")
            target = first + offset
            for column, value in row.items():
                if not isinstance(column, str) or not COLUMN.fullmatch(column):
                    raise pack.PackError(f"table row {offset + 1} column {column!r} is not a column letter")
                if value is None:
                    raise pack.PackError(f"table row {offset + 1} column {column} is null; omit the cell instead")
                wb[sheet][f"{column}{target}"] = scalar(value, f"{sheet}!{column}{target}")
                written += 1
    return written


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--template", required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--context", required=True)
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--manifest")
    parser.add_argument("--sample", action="store_true")
    args = parser.parse_args()

    try:
        template = Path(args.template)
        profile_path = Path(args.profile)
        context_path = Path(args.context)
        output = safe_output(Path(args.output_dir), [template, profile_path, context_path])
        profile = pack.load_profile(profile_path)
        if profile["format"] != "xlsx":
            raise pack.PackError(f"render-sheet needs an xlsx profile, got {profile['format']!r}")
        manifest = pack.load_manifest(Path(args.manifest) if args.manifest else None, profile)
        context = pack.read_json(context_path, "context")
        if set(context) - {"cells", "tables"}:
            raise pack.PackError("context admits only 'cells' and 'tables'")
        cells = context.get("cells", {})
        tables = context.get("tables", [])
        if not isinstance(cells, dict):
            raise pack.PackError("context cells must be an object keyed by Sheet!Cell")
        if not isinstance(tables, list):
            raise pack.PackError("context tables must be a list")
        if not cells and not tables:
            raise pack.PackError("context has nothing to write")

        from openpyxl import load_workbook

        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            # rich_text keeps untouched mixed-format runs intact on save.
            wb = load_workbook(template, rich_text=True)
            written = apply_cells(wb, profile, cells) + apply_tables(wb, profile, tables)

            with tempfile.TemporaryDirectory(prefix="business-sheet-", dir="/tmp") as staging_dir:
                staging = Path(staging_dir)
                xlsx = staging / "document.xlsx"
                exclusive_write(xlsx, wb.save)
                lost = sorted(
                    {str(w.message) for w in caught if UNSUPPORTED.search(str(w.message))}
                )
                if lost:
                    raise pack.PackError(
                        "the template uses features openpyxl drops: " + "; ".join(lost[:3])
                    )
                dropped = pack.missing_parts(template, xlsx)
                if dropped:
                    raise pack.PackError(
                        "the template carries parts openpyxl cannot preserve "
                        f"({', '.join(dropped[:8])}); this workbook is not safe to fill here"
                    )
                pack.convert_pdf(xlsx, output, "document")
                published = publish_staged_file(xlsx, output, xlsx.name)
                pdf = output / "document.pdf"
                if not pdf.is_file() or pdf.stat().st_size == 0:
                    raise pack.PackError("LibreOffice did not publish the PDF")

        report = {
            "schema": SCHEMA,
            "kind": "render-sheet",
            "sample": bool(args.sample),
            "manifest": manifest,
            "profile_revision": profile.get("revision"),
            "format": profile["format"],
            "cells_written": written,
            "parts_preserved": True,
            "files": sorted([published.name, pdf.name]),
        }
        (output / "report.json").write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        print(json.dumps({"status": "rendered", "cells": written, "files": sorted(p.name for p in output.iterdir())}))
        return 0
    except pack.PackError as exc:
        print(f"Render blocked: {exc}", file=sys.stderr)
        return 2
    except Exception as exc:  # a malformed workbook is a refusal, not a partial book
        print(f"Render blocked: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
