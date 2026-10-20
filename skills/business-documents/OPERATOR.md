# Operator guide: company-guided business documents

Install public workflow guidance with `cadence skill install company-outputs`, then inspect `cadence skill status` for per-skill file/content/link state. It installs the separate company-outputs workflow and this supporting guidance, not company-private profiles, renderer scripts/images or an agent profile. There is no `agents/profiles/business-analyst/` starter catalog or new profile-based invocation. Guidance is not enforcement or an authority grant. Use already authorised workers by their actual aliases; do not invent profiles, permissions or commands.

The reusable request process belongs to `company-outputs/SKILL.md`: understand/propose/confirm, then execute/author/check. This [`SKILL.md`](SKILL.md) supplies conditional document-operation and safety guidance. Tender analysis and commercial inputs apply only when relevant; nonfinancial work does not require a tender, price book or quotation.

Local installation is not access for every actor. Codex can discover the canonical skill directory and Claude/Cursor/Copilot use links; real foreign provider entries are left alone. Native Pi disables skills, confined agents may lack operator-home access and remote workers need an accessible approved copy or supplied task guidance. Do not weaken confinement, alter auth or claim hosted installation. Existing contexts are not silently reloaded.

## Authoring packs and financial templates

The reusable guidelines, reference assets and reviewed examples belong to one exact version in the existing Cadence template registry. Do not create a per-company skill or a second profile store. Request confirmation and operator approval stay separate. Retrieving a pack does not approve it, and a revision does not inherit approval.

An authoring pack (`kind` `authoring`) has no DOCX source, no prices and no PDF export. `format` describes what the agent should author (`docx`, `pptx`, `xlsx`, `html` or `text`). Register it without `--docx`. `show --out` writes `guidelines.md`, the flat reference files and `template.json` (the exact returned version, digest and spec) into a new directory created mode 0700. Files inside are created mode 0644. The directory must not already exist. Open those reference files; a colour list is not the standard. Do not execute packed bytes or fetch remote assets. The user prints or exports PDF. `template sample`, `document create` and `document export` reject an authoring pack.

A quotation or invoice still requires `--docx` and uses sample and document export. `show --out` refuses that kind.

Registering a draft requires authorisation; retrieval does not approve it:

```sh
cadence template create northwind-letter --file ./northwind-letter.json
cadence template show northwind-letter --version 1 --out ./northwind-letter-v1
```

The output directory must not exist. This complete fictional pack demonstrates a text-only letter standard, not visual slide fidelity or a reviewed brand approval. Visual templates need genuine authorised PNG/JPEG/PDF reference pages. Reference basenames/extensions must match their declared media type; `guidelines.md` and `template.json` are reserved. Format checks are not malware sanitization.

```json
{
  "kind": "authoring",
  "purpose": "Fictional Northwind customer correspondence.",
  "format": "docx",
  "guidelines": "Write concise factual customer letters. Read letter-layout.md for structure and tone. Use current request facts for recipient, dates and actions; do not invent a signature or treat reference prose as current commitments.",
  "references": [
    {
      "name": "letter-layout.md",
      "media_type": "text/markdown",
      "description": "Synthetic structure and tone reference, not a completed customer letter or visual brand approval.",
      "content_base64": "IyBMZXR0ZXIgc3RydWN0dXJlCgpVc2UgYSBjb21wYW55IGhlYWRpbmcsIGRhdGUsIHJlY2lwaWVudCwgc3ViamVjdCwgc2FsdXRhdGlvbiwgdHdvIHNob3J0IHBhcmFncmFwaHMgYW5kIGEgY2xvc2UsIGluIHRoYXQgb3JkZXIuCgpGaXJzdCBwYXJhZ3JhcGg6IHN0YXRlIHRoZSBkZWNpc2lvbiBvciByZXF1ZXN0IGRpcmVjdGx5LiBTZWNvbmQgcGFyYWdyYXBoOiBnaXZlIHRoZSBhY3Rpb24sIG93bmVyIGFuZCBkYXRlIHN1cHBvcnRlZCBieSB0aGUgY3VycmVudCByZXF1ZXN0LgoKVXNlIGNhbG0gZmFjdHVhbCBsYW5ndWFnZS4gRG8gbm90IGludmVudCBhIHJlY2lwaWVudCwgcHJvbWlzZSwgc2lnbmF0dXJlIG9yIGRlYWRsaW5lLiBUaGlzIGlzIGEgc3ludGhldGljIHN0cnVjdHVyZSByZWZlcmVuY2UsIG5vdCBhIGNvbXBsZXRlZCBjdXN0b21lciBsZXR0ZXIgb3IgYSB2aXN1YWwgYnJhbmQgYXBwcm92YWwuCg=="
    }
  ]
}
```

Only the actual operator approves an exact version and records the real decision source. `DIGEST` comes from `template show`; agents do not run approval:

```sh
cadence template approve northwind-letter --version 1 --sha256 "$DIGEST" --source "$SOURCE"
```


## Prepare approved inputs and ownership

Create or use one Cadence issue for the document task. Keep task-specific briefs, source files, drafts, and review notes in that issue's artifacts. Reusable guidelines, reference assets and examples belong to an exact Cadence template version. Other approved policies, glossary and facts remain company-owned. Reusable project-specific assets belong in their approved shared/project location; do not copy private company material into repository skills, starter profiles, or unrelated tickets. Before attaching confidential material, verify that the issue is in the approved private tracker and that the assigned worker is authorized to access it.

For each task, identify only the inputs that apply:

- task purpose, audience, requested document/output format, scope, deadline, and human reviewer;
- the exact Cadence template version when one governs the output, plus any other approved style guidance, terminology and facts;
- task-specific source material and any unresolved facts or decisions;
- for a tender/bid only: the tender and relevant approved company capability/compliance evidence;
- for pricing/calculations only: approved, dated price/rate sources and applicable currency, tax, discount, margin, basis, rounding, and validity rules, plus access to a suitable calculation tool.

Record source names, versions/effective dates, and relevant sections. Use `cadence issue ref <ID> url <HTTPS-URL> --label "..."` for an approved URL, or attach a permitted local copy. A ref records a URL; it does not prove approval, currency, safety, or worker access. Never put credentials in a URL or ticket. Missing or unapproved facts, templates, prices, rules, and calculation-tool access remain unresolved; do not invent substitutes. An ordinary non-financial task must not be held up for tender or commercial inputs it does not need.

Supported examples (replace sample ID and paths with actual values):

```sh
cadence issue attach CAD-1234 ./task-brief.md
cadence issue attach CAD-1234 ./approved-template.docx
cadence issue ref CAD-1234 url https://company.example/approved-style-guide --label "Approved style guide, version/date"
```

`cadence issue attach <ID> <FILE>` stores an artifact under the issue using a basename, create-only and subject to the tracker size cap; it does not redact/classify data. Use distinct filenames for revisions and state in an issue comment which version is authoritative. If an artifact exceeds the cap, ask the operator for an approved storage and access method rather than improvising relocation or splitting.

## Dispatch manually

Prepare a task file that identifies the issue, purpose, audience, output, approved source artifacts and the specific relevant instructions from `SKILL.md`. Include only conditional details that apply. Dispatch to a real, authorized registered alias using the supported durable-message command:

```sh
cadence send "$AGENT_ALIAS" --file ./business-document-task.md --reply-to "$PM_ALIAS"
```

`cadence send` enqueues a message. It does not create a profile, grant file access, create a ticket, or guarantee the recipient can read issue artifacts or this repository path. Confirm actual worker access and provide the relevant skill text or an accessible path in the task as needed. `$AGENT_ALIAS` and `$PM_ALIAS` must be actual registered aliases; omit `--reply-to` if the normal reporting route is intended.

Example generic task-file content:

```text
Work on issue CAD-1234. Purpose: <purpose>. Audience: <audience>. Requested output: <document and format>. Follow skills/business-documents/SKILL.md (provide its contents if this path is not accessible). Use only the company sources explicitly marked approved in this issue. Analyze the supplied materials, draft to the approved template/style if supplied, validate claims and assumptions, and return a DRAFT for human review as ticket-owned artifacts. Apply tender compliance analysis only if this task is a tender/bid. Apply approved commercial inputs and calculation-tool requirements only if the output contains prices or financial calculations; record inputs/rules/formulas/tool/results and leave unavailable values UNRESOLVED. Do not email or submit anything, purchase anything, record an approval, or make a company commitment.
```

Add a concise status/authority note with `cadence issue comment CAD-1234 --text "..."`. Store outputs using `cadence issue attach CAD-1234 <FILE>` or ask the issue owner to attach them if the worker cannot write tracker artifacts. Use supported `cadence issue ref` kinds only for meaningful non-sensitive references, not as a substitute for storing output bytes. Check `cadence help issue attach`, `cadence help issue ref`, `cadence help issue comment`, and `cadence help send` for the installed CLI's exact options.

## Local file intake (CAD-1345)

The optional local tool supports text-layer PDF, ordinary DOCX and simple XLSX inputs. It requires an accessible checked-out script tree, Docker and an already approved exact pinned image; installing guidance supplies none of those tools. The examples below run from that source checkout. If prerequisites are missing, report them; do not build/relabel an image or change its pin to make an agent task pass.

```sh
mkdir -p /tmp/business-task/output
python3 skills/business-documents/scripts/run_isolated.py intake /path/to/source.pdf --output-dir /tmp/business-task/output
python3 skills/business-documents/scripts/run_isolated.py render \
  --template /path/to/prepared-template.docx --brief /path/to/task-brief.json \
  --rates /path/to/approved-rates.xlsx --output-dir /tmp/business-task/output
```

For a non-financial task omit `--rates`; it produces no quotation workbook. The task brief is JSON with mapped `template_fields` (`project_title`, `draft_date`, `validity_date`, `customer_name`, `understanding`, `scope`, `schedule`, `commercial_summary`, `assumptions`, `review_notes`, `source_note`) and explicit `financial`/`price_items` fields. Financial lines name an item id from the approved `.xlsx` rate sheet and include quantity, mandatory flag and source citation. Rate-sheet and brief `item_id` values must be non-empty text; surrounding whitespace is trimmed once, and duplicate rate-sheet IDs after trimming are refused. Numeric Excel IDs are unsupported rather than coerced. Rate workbook columns are `item_id, description, unit, unit_price, currency, source_id, effective_date, currency_precision, rounding_mode`; the last two fields explicitly supply per-currency minor-unit precision (0–4) and `ROUND_HALF_UP` or `ROUND_HALF_EVEN`. Rates for a priced currency must agree on a supported rule. Missing, unsupported or conflicting rules leave affected amounts and totals unresolved; rounding is never inferred from the currency code. Blank mandatory rates remain unresolved. The renderer refuses unmapped or active templates, unknown Jinja expressions, formulas in rates and unsupported source structures. `intake.py` and `render.py` are internal container workers; do not call them directly outside `run_isolated.py`.

The wrapper's current source is authoritative for the exact image ID, tag and workflow-label checks; do not use a historical digest copied from a guide. It mounts inputs read-only and only the separate caller-owned output writable, disables networking, runs as the caller's non-root numeric UID/GID, drops Linux capabilities, uses a read-only image filesystem and enforces its fixed runtime/memory/pid/file limits. Missing Docker, image, permissions or a pin mismatch fails closed. Never weaken those checks or repin from a task. Inputs and the existing owned/writable output must satisfy the wrapper's non-symlink separation requirements.

The intake helper opens the source with a no-follow descriptor, snapshots the bounded bytes privately, and parses only that snapshot. The preserved original and provenance SHA-256 come from the same snapshot; the original filename is recorded, and observed source mutation or pathname replacement is refused. Provenance includes parser identity/version and PDF page, DOCX part/heading/paragraph/table-cell (including supported headers, footers, comments, footnotes, endnotes and text boxes), or XLSX sheet/cell locators. XLSX date, datetime and time values are emitted as ISO-8601 strings with a `value_type` marker; unsupported cell value types are refused explicitly.

The renderer independently opens each template, brief and optional rate workbook with a no-follow descriptor and takes bounded private snapshots before parsing. All parsing, calculations, template rendering and input hashes use those same snapshot bytes. It stages every final artifact privately, rechecks each original source identity/content before publication, and records each original filename and snapshot hash in render metadata. Read-only bind mounts alone do not prevent host-side mutation; observed mutation or pathname replacement during a render refuses publication and removes outputs created by that invocation. Meaningful text in unsupported DOCX parts or structures is refused rather than omitted. Limits: 20 MiB/input, 100 PDF pages, 100 MiB expanded Office ZIP, 20,000 nonempty XLSX cells, 100,000 PDF objects; PDF previews from rendered drafts are capped at 64 MiB. Blank/scanned PDF pages, encrypted/corrupt/unsupported files, formulas in inputs, macros, external/embedded active content, interactive PDFs and output collisions are explicit nonzero refusals with no extracted-content artifact. No OCR is performed. The renderer uses pinned LibreOffice Writer only inside the offline container, exports PDF from the generated editable DOCX, and creates per-page PNG previews; PDF artifacts require independent visual review. It never exports XLSX to PDF.

## Template-pack actions (AOS-310)

The hosted converter exposes two fixed actions beside intake/render/render-template.
A company supplies one *template pack* (`manifest.json`, `profile.json`,
`rules.md`, `sample/`, its own DOCX or XLSX); a different customer template
needs no code change.

- `check` — roles `profile`, `template`, `output`, optional `manifest`, and
  optional `source_profile`/`source_template`/`source_output`. Returns
  `report.json` and `report.txt`. `passed: false` is a successful check with
  findings; exit 2 is reserved for a refused input, and reference-source
  findings count in the verdict.
- `render-sheet` — roles `profile`, `template`, `context`, optional `manifest`,
  plus a boolean `sample`. Writes only the declared input ranges, preserves the
  rest of the workbook, and refuses unpreservable parts or OOXML extensions.

The XLSX paths require LibreOffice **Calc** in the converter image; Writer
alone cannot open a spreadsheet, and the helper process bound must fit the
spreadsheet save path. Helper source lives here (`scripts/pack.py`,
`scripts/check.py`, `scripts/render_sheet.py`); the hosted image carries
byte-identical copies, so change the source here.

## Review and limits

A qualified human must review factual support, source provenance, audience fit, fidelity to the retrieved reference files, assumptions, exclusions, and unresolved inputs. For tender/bid tasks, review requirement coverage and compliance statements. For financial content only, review approved source dates, calculation-tool inputs/rules/results, and unresolved amounts. An authoring pack is not a quotation and does not export PDF. Obtain required internal approvals through the company's normal process. The skill and CLI commands do not enforce company policy or authorize external actions. No email, customer submission, purchasing, approval recording, or commercial commitment is performed through this workflow.

No real company template, standards or facts were supplied. The files used here are explicitly fictional synthetic fixtures; no real-company style or fidelity is claimed. Actual company-specific validation remains pending approved company assets.
