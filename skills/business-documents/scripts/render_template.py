#!/usr/bin/env python3
"""Render one company DOCX template with a validated draft context to DOCX and PDF (CAD-1393).

The context is produced by the daemon after validation and deterministic
calculation; this script only substitutes placeholders and converts. It
never reads business rules, and any placeholder the context does not
provide is refused (StrictUndefined).
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from jinja2.sandbox import SandboxedEnvironment
from jinja2 import StrictUndefined

from docxtpl import DocxTemplate

from intake import preflight_office
from render import exclusive_write, publish_staged_file, safe_output

STEM = "document"
SAMPLE_BANNER = "SAMPLE PREVIEW - placeholder values, not company data, not a document to send"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--template", required=True, type=Path)
    parser.add_argument("--context", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--sample", action="store_true", help="label the output as a sample preview")
    args = parser.parse_args()
    try:
        output = safe_output(args.output_dir, [args.template, args.context])
        context = json.loads(args.context.read_text(encoding="utf-8"))
        if not isinstance(context, dict):
            raise ValueError("context must be a JSON object")
        preflight_office(args.template, "docx").close()
        with tempfile.TemporaryDirectory(prefix="business-template-", dir="/tmp") as staging_dir:
            staging = Path(staging_dir)
            docx = staging / f"{STEM}.docx"
            template = DocxTemplate(str(args.template))
            # The sandboxed environment blocks attribute and dunder access from template text.
            template.render(context, jinja_env=SandboxedEnvironment(undefined=StrictUndefined, autoescape=True))
            if args.sample:
                # Banner on the rendered document that save() writes; get_docx() is not it.
                document = template.docx
                if document.paragraphs:
                    document.paragraphs[0].insert_paragraph_before(SAMPLE_BANNER)
                else:
                    document.add_paragraph(SAMPLE_BANNER)
            exclusive_write(docx, template.save)
            with tempfile.TemporaryDirectory(prefix="cadence-lo-", dir=staging_dir) as converter_dir:
                command = ["soffice", "--headless", f"-env:UserInstallation=file://{converter_dir}/profile",
                           "--convert-to", "pdf", "--outdir", converter_dir, str(docx)]
                subprocess.run(command, check=True, capture_output=True, text=True, timeout=85)
                converted = Path(converter_dir) / f"{STEM}.pdf"
                if converted.is_symlink() or not converted.is_file() or converted.stat().st_size == 0:
                    raise ValueError("LibreOffice did not create the expected PDF in private staging")
                pdf = staging / f"{STEM}.pdf"
                pdf.write_bytes(converted.read_bytes())
            published = [publish_staged_file(docx, output, docx.name), publish_staged_file(pdf, output, pdf.name)]
        print(json.dumps({"status": "rendered", "files": [path.name for path in published]}))
        return 0
    except Exception as exc:
        print(f"Render blocked: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
