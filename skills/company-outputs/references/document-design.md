# Document design

Load this when the deliverable has a company visual standard: a proposal, report, letter, slide, or a review of quotation or invoice presentation. Skip it when the task has no design reference. It is not a per-company skill, a renderer, a PDF service, a schema change, or authority to revise a template, approve it, issue it, or send it.

Company references govern appearance. Do not prescribe a house font, palette, or layout the files do not show, and do not borrow another company's defaults.

## Inspect, then record a style guide

For an authoring pack, read the exact version's guidelines and inspect its actual reference files — images, PDF pages, and text or markdown — across the relevant cover, body, table, dense, and closing layouts. A colour list or prose summary cannot replace those files. Approved guidelines set requirements; examples demonstrate them, not silently override them. If a file is missing, unreadable, or conflicts with the guidelines, record the gap rather than inventing a stand-in.

When defining or revising a standard, put a short human-readable style guide in the authoring pack's guidelines, not a second profile registry or new template fields. For a version already stored, leave its bytes unchanged: use the derived observations for this request and propose an ordinary authoring-pack revision if needed. Registration and revision keep their own confirmation. Neither approves the template. A packed example is not this request's facts; author fresh content.

Reuse an adequate existing guide; do not redefine the company's style on every request. Analyse new references or gaps only as needed for the selected output.

A financial template (`kind` `quotation` or `invoice`) does not gain authoring guideline or reference fields. Its presentation standard is the existing approved DOCX. A separately selected company reference pack may inform a review of that DOCX or its export. It is not the financial renderer and it is not approval of the financial version. Do not write authoring guidelines into a financial template or change a financial template or retained record from this guide. Any change to that DOCX or its spec is a financial revision and still needs the required operator approval.

Label every choice:

- **Observed** — visible in a named reference, with the file and where on it.
- **Inferred** — a repeatable rule generalised from more than one observation. Drop it if a later page contradicts it.
- **Proposed** — an authoring choice where the standard is silent, such as a physical print size when only an aspect ratio is known. Disclose it; return material design-direction changes to confirmation.
- **Unresolved** — references disagree, or the asset cannot be opened. Record the gap and resolve material design choices with the user; do not silently average conflicting standards.

## Layout grammar

Record the supported grammar and distinguish proposed adaptations from source observations:

- **Page** — size, orientation, grid, margins, columns.
- **Type** — roles (title, heading, body, caption, table, footer), relative size, weight, case, and line length. Name a typeface only when a reference uses it and that licensed file is available to the authoring environment. Otherwise record the role and the available substitute.
- **Colour** — roles (ground, ink, accent, rule, table) and the observed values.
- **Spacing** — stack rhythm, paragraph and section gaps, indent.
- **Images** — crop, alignment, caption, and whether they bleed.
- **Tables** — header treatment, rules, numeric alignment, row density.
- **Headers and footers** — what repeats, and what changes by page.
- **Page rhythm** — how a section opens and closes, where a new page starts, and how much of a page may stay empty.
- **Dense and long output** — how the references keep a long table or narrative readable (continued headings, split points, notes). Compare the real output at that length, not only the cover.

## Complete printed surfaces

Every part of the printed or exported surface has to be readable on its page. Two defects survive every structural check and appear only on the rendered pages, so inspect them there:

- **A block that has to be read as a unit.** A summary, totals, reconciliation or signature block that splits across pages loses its row labels, and with them its meaning, on the continuation page. Set that block's print area to fit one page wide and one page tall where the format supports it, keeping the text readable, so it prints on one page. When the governing form is mandatory and cannot be changed, report the limitation: a change to an approved template or a client-mandated form is its own revision, not a layout repair.
- **A built index.** A document index (table of contents) exports as the template's field placeholder (`Right-click here and choose Update Field`) unless fields and indexes are refreshed as part of the export. Build it before export, and treat the placeholder reaching the reader as a defect.

Reopening the file, counting pages or checking bounds shows neither one. Check the actual pages as the sections below require, and state which checks could not be run.

## Proposals and other authoring formats

An authoring pack does not render DOCX, prices, or PDF. Author the confirmed format. The user prints or exports the final PDF.

When that format is HTML, use semantic structure and CSS in the document itself. There is no sample framework to copy.

- Screen and print differ. Set the printed page size, orientation, and margins with `@page` from the observed page. Put pagination in `@media print`. Use `@media screen` only for on-screen reading width. Screen layout does not decide the printed page.
- Keep content in normal flow. Break before a section rather than squeezing it. Let a table or figure move to the next page. Repeat the table header on each printed page.
- Reflow or split content that does not fit. Do not clip it with a fixed height and `overflow: hidden`.
- Fonts and images must be loaded before the page is judged. A missing required font or image is a defect. Prefer bundled or local authorised assets for reproducible printing. A URL in a reference grants no fetching authority; any external asset operation needs separate authorised scope.
- Browser print adds its own chrome, often adds margins, may scale away from 100%, and often omits background colours until the user enables them. Record which of those the check used.
- For QA, print the actual HTML to a temporary PDF in the browser and inspect those pages, including a long section. Label the file temporary QA evidence. It is not the issued document. The user controls final printing and export, including scale and backgrounds. Do not add a PDF service.

For DOCX, PPTX, XLSX, or text, apply the same grammar in that format's real styles. Inspect the selected output, not an HTML stand-in. Reopening a file or checking bounds is not a visual check.

## Quotations and invoices

Use the financial path for amounts and for the file the reader sees: the approved quotation or invoice DOCX, the confirmed native calculation, the retained draft, and DOCX plus PDF export. That approved DOCX is the presentation standard. Do not author an HTML quotation or invoice as a substitute renderer, and do not treat a company reference pack as one. Shared print judgement below is a review of that exported surface. Where the DOCX path cannot repeat a header, avoid a clip, or match a reference-pack observation, report the limitation. Matching the pack is not a reason to edit the approved template or the retained draft in place.

Styling does not create facts. Identifiers, dates, status, currency, tax, discounts, totals, and payment details come from the authorised commercial inputs and the native calculation. Do not invent a number, a paid state, or a bank detail.

On the actual exported pages, check:

- Identifiers, dates, status, and payment instructions match their provenance.
- Each displayed amount matches the native result: line extensions, selected subtotal, discount, tax, and total. A styling pass does not recalculate.
- A long item list stays readable: the header row repeats, a figure stays with its description, and totals remain with their labels. Report clipping, overlap, or a dropped repeating header instead of hiding it.

Layout repair stays in Stage 2. A change to price, selection, status, or payment returns to confirmation. Nothing here grants approval, numbering, issue, or send.
