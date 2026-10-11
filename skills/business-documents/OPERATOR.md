# Operator guide: company-guided business documents

This is a manual, ticket-scoped drafting workflow. The checkout has no `agents/profiles/business-analyst/` starter catalog or supported profile-based invocation, so this guide uses only the existing durable-message and issue-artifact commands. Do not treat this skill as an installed profile, enforcement mechanism, or authority grant. Use an already registered, authorized worker by its actual alias, or ask the operator to arrange one; do not guess aliases, register a substitute profile, or invent commands.

The reusable process is in [`SKILL.md`](SKILL.md). It covers proposals, quotations, business reports, and client correspondence as examples of one shared brief-to-draft workflow. Tender analysis is conditional on a tender/RFP/bid task. Commercial sources and calculation tools are required only when requested output contains prices or financial calculations; non-financial work does not need a tender, price book, or quotation.

## Prepare approved inputs and ownership

Create or use one Cadence issue for the document task. Keep task-specific briefs, source files, drafts, and review notes in that issue's artifacts. Reusable approved company standards, policies, glossary, facts, templates, and examples remain company-owned. Reusable project-specific assets belong in their approved shared/project location; do not copy private company material into repository skills, starter profiles, or unrelated tickets. Before attaching confidential material, verify that the issue is in the approved private tracker and that the assigned worker is authorized to access it.

For each task, identify only the inputs that apply:

- task purpose, audience, requested document/output format, scope, deadline, and human reviewer;
- approved company style guidance, terminology, facts/evidence, and the designated template, if one exists;
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

The optional local workflow supports text-layer PDF, ordinary DOCX, and simple XLSX inputs. Build the pinned dependency image from the repository root; packages are installed inside that image only, never on the host:

```sh
docker build --pull=false -f skills/business-documents/scripts/Containerfile --tag cadence-business-files:cad-1345-v1 .
mkdir -p /tmp/business-task/output
python3 skills/business-documents/scripts/run_isolated.py intake /path/to/source.pdf --output-dir /tmp/business-task/output
python3 skills/business-documents/scripts/run_isolated.py render \
  --template /path/to/prepared-template.docx --brief /path/to/task-brief.json \
  --rates /path/to/approved-rates.xlsx --output-dir /tmp/business-task/output
```

For a non-financial task omit `--rates`; it produces no quotation workbook. The task brief is JSON with mapped `template_fields` (`project_title`, `draft_date`, `validity_date`, `customer_name`, `understanding`, `scope`, `schedule`, `commercial_summary`, `assumptions`, `review_notes`, `source_note`) and explicit `financial`/`price_items` fields. Financial lines name an item id from the approved `.xlsx` rate sheet and include quantity, mandatory flag and source citation. Rate workbook columns are `item_id, description, unit, unit_price, currency, source_id, effective_date, currency_precision, rounding_mode`; the last two fields explicitly supply per-currency minor-unit precision (0–4) and `ROUND_HALF_UP` or `ROUND_HALF_EVEN`. Rates for a priced currency must agree on a supported rule. Missing, unsupported or conflicting rules leave affected amounts and totals unresolved; rounding is never inferred from the currency code. Blank mandatory rates remain unresolved. The renderer refuses unmapped or active templates, unknown Jinja expressions, formulas in rates and unsupported source structures. `intake.py` and `render.py` are internal container workers; do not call them directly outside `run_isolated.py`.

The wrapper verifies local image ID `sha256:0f95114a36daad9b02f0f879eac19c616d7fbd9aec02f9aadc1499d81fb73c00` and CAD-1345 labels, mounts each input file read-only, mounts only the task output directory writable, disables networking, runs as the caller's non-root numeric UID/GID, drops Linux capabilities, uses a read-only image filesystem, and bounds runtime to 90 CPU seconds/105 wall seconds, memory to 1 GiB, pids to 64, temporary storage to 128 MiB and each file to 100 MiB. If a rebuild changes the image ID, the wrapper fails closed until the inspected and validated ID is explicitly pinned. Missing Docker/image/permissions fail closed. The owned fixture output and inputs must be separate, non-symlink paths; the output directory must already exist and be owned/writable by the caller.

The intake helper opens the source with a no-follow descriptor, snapshots the bounded bytes privately, and parses only that snapshot. The preserved original and provenance SHA-256 come from the same snapshot; the original filename is recorded, and observed source mutation or pathname replacement is refused. Provenance includes parser identity/version and PDF page, DOCX part/heading/paragraph/table-cell (including supported headers, footers, comments, footnotes, endnotes and text boxes), or XLSX sheet/cell locators. Meaningful text in unsupported DOCX parts or structures is refused rather than omitted. Limits: 20 MiB/input, 100 PDF pages, 100 MiB expanded Office ZIP, 20,000 nonempty XLSX cells, 100,000 PDF objects; PDF previews from rendered drafts are capped at 64 MiB. Blank/scanned PDF pages, encrypted/corrupt/unsupported files, formulas in inputs, macros, external/embedded active content, interactive PDFs and output collisions are explicit nonzero refusals with no extracted-content artifact. No OCR is performed. The renderer uses pinned LibreOffice Writer only inside the offline container, exports PDF from the generated editable DOCX, and creates per-page PNG previews; PDF artifacts require independent visual review. It never exports XLSX to PDF.

## Review and limits

A qualified human must review factual support, source provenance, audience fit, template/style fidelity, assumptions, exclusions, and unresolved inputs. For tender/bid tasks, review requirement coverage and compliance statements. For financial content only, review approved source dates, calculation-tool inputs/rules/results, and unresolved amounts. Obtain required internal approvals through the company's normal process. The skill and CLI commands do not enforce company policy or authorize external actions. No email, customer submission, purchasing, approval recording, or commercial commitment is performed through this workflow.

No real company template, standards or facts were supplied. The files used here are explicitly fictional synthetic fixtures; no real-company style or fidelity is claimed. Actual company-specific validation remains pending approved company assets.
