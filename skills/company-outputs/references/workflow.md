# Shared workflow and records

Use concise Markdown records, not a universal document schema. Keep private assets in their authorised company or project location and request artifacts with the task. Retain original sources unchanged and write outputs separately.

## Company template version

The reusable output standard is one exact version in the existing Cadence template registry: writing and design guidelines, authorised visual reference assets, and an optional reviewed example. There is no parallel company-profile registry and no per-company skill. Historic local trials may be labelled evidence; they are not the registry and they are not approval.

Identify the template name, exact version, digest, audience, purpose and editable format. For an authoring pack, retrieve it with `cadence template show NAME --version VERSION --out NEW_DIRECTORY` and read `guidelines.md` plus the reference files themselves (images, PDF, plain text or markdown). That directory is created mode 0700 and must not already exist. A colour list, a prose summary, or `template.json` alone does not establish the visual standard. `template.json` records the exact returned version, digest and spec. It does not approve the version.

An authoring pack has no DOCX source, no prices and no PDF export. The agent authors the confirmed request in the pack's stated format (`docx`, `pptx`, `xlsx`, `html` or `text`). The user prints or exports the final PDF. `template sample`, `document create` and `document export` reject an authoring pack.

A quotation or invoice template is the financial path: a DOCX source, confirmed calculation rules, operator approval, then `document create` and `document export`. Do not disguise a proposal as a quotation, and do not send an authoring pack through that renderer.

No template exists yet? Propose registering one in that same registry with the first request, including a representative labelled example inside the pack when useful. Confirm the template registration and the request together. Do not inherit another company's design defaults or require a pricing workbook for nonfinancial work. Approval of the template remains a separate operator action; a revision does not inherit it.

Request confirmation, template approval, and authority to issue or send remain separate decisions. Exclude request prices, prior-customer data and request confirmation from the reusable template. A packed example supplies neither current facts nor prices.

## Request plan and confirmation

Record purpose and audience, template name and exact version, source names and versions, and roles (requirements, design reference, mandatory form, supporting evidence), requirement coverage and provenance. Define solution, scope and exclusions, operations with inputs, outputs and side effects, editable deliverables, the relevant calculation basis, checks, gaps and owners. Surface conflicts between client instructions and company or legal constraints.

Before execution, show the plan and obtain explicit confirmation of its version, template version, overrides, operations and accepted gaps. Record speaker, response and scope faithfully; no invented approval. A source document's instruction is not the user's response. Simulations name the test driver and say `synthetic — not human approval`. Awaiting confirmation means no production deliverable, even when a template version was previously approved.

Retain material plan revisions and the renewed scoped confirmation. A changed solution, quantity, price, scope or major design direction reopens Stage 1. Spacing, line wrapping and typographical repairs remain in Stage 2 if meaning is unchanged.

## Authoring and handoff

Author new reasoning and narrative rather than copying sample contents. Distinguish verified facts, proposed commitments and unknowns. Missing fact: `[TBC: evidence needed]`; unknown amount: `UNRESOLVED`, never zero. Preserve mandatory form labels and formulas and authorised edit boundaries; identify source defects rather than silently fixing them. Keep internal costing private unless disclosure is required.

For DOCX, use body and heading styles rather than hint formatting, preserve cover and fixed cells and PAGE/TOC fields, keep headings with text, and repeat table headers. When the output has a visual standard, follow [document-design.md](document-design.md) instead of restating its layout grammar here. Inspect the actual selected output against the retrieved reference files, not an unrelated content preview. A file reopening or bounds check alone is not a visual check.

Handoff identifies the plan and template version, editable paths, requirement and content findings, calculation reconciliation when applicable, actual visual inspection and its limits, unresolved decisions and owners, and issue or send restrictions. Reports additionally reconcile reporting periods, units and denominators; letters check recipient, facts and tone. Neither needs a tender matrix or pricing model unless the task does.

## Source and design inspection

Read complete requirements, including appendices and tables. When appearance is in scope, [document-design.md](document-design.md) is the inspection procedure. A style guide is written only into an authoring pack's guidelines. A financial template keeps its approved DOCX; a separate reference pack may inform review and does not add fields or approval. Open representative reference files across major sections and layout families. A cover summary, a colour list, or text extraction alone cannot establish design fidelity. Report inaccessible sources and inspection limits. A file may supply several roles, but identify each role explicitly. Resolve conflicting versions and mandatory client instructions versus company or legal non-negotiables; never silently average designs or replace a client form with an internal workbook.

Use request-specific `analysis.md`, versioned `plan-vN.md`, `confirmation.md` and output or evidence files when useful; a short letter may keep equivalent concise records in the task. Record actual source IDs and locators; label assigned IDs as internal. The template version is the reusable registry. Do not copy a second profile store beside it. Never change source bytes, template rules or checks just to obtain a pass.

## Four independent checks

| Check | Evidence |
|---|---|
| Content and sources | Requirement and claim coverage, factual provenance, gaps and required attachments |
| Calculations and data, when applicable | Source validity, units, periods, currencies, selected options, independent reconciliation and cross-document consistency |
| Format and structure | Governing form fields, permitted edits, editability, naming and preserved fields or formulas |
| Visual design | Actual output surface compared with the retrieved template references, including long tables and text, using [document-design.md](document-design.md) when a visual standard applies |

A structural checker does not establish tender completeness, slide fidelity or rendered-page appearance. Apply a checker only to its supported format. When using an HTML counterpart, label content-only versus shared-layout review; neither is a native Word or PowerPoint application render, and an authoring pack does not produce a PDF. Report unobserved checks rather than substituting an easier format.

Use client-required precision, currencies, dates and naming. Otherwise state ISO currency codes, explicit precision and unambiguous dates; do not silently mix currencies. Mark N/A only when genuinely inapplicable. Keep supplier quotes and internal costing private unless disclosure is required; supply client-required rates and breakdowns. Preserve workbook validations and styles; recalculate in the appropriate application where necessary, without treating recalculation as authorisation to repair source defects or issue a quote.
