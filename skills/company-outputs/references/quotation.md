# Quotation and invoice authoring and reconciliation

Use this reference only when the output contains prices, options or financial totals.

## Which path

- **Issuable:** the company's approved financial template, its confirmed calculation and a retained draft with export (`references/cadence.md`). Where one exists, it governs what the customer finally receives.
- **Draft view:** an HTML view of a priced draft, shown for review. Use the Render capability when this session has it. Otherwise author the HTML and take every amount from the Calculate capability. Label it `SAMPLE` when no approved template stands behind it, or `SUGGESTED — NOT APPROVED — HUMAN REVIEW REQUIRED` when one does. It gets no document number and is not the retained or issued document. Use no scripts and no remote files: embed logos and fonts in the document itself.

An authoring pack is not a quotation: it has no line items and no calculation rules.

## Inputs

Record each selected item's quantity and unit, price, currency, dated approved source, scope and option status. Establish tax basis, discount order, precision, rounding and validity or terms from the governing commercial inputs. Never assume a tax rate or currency: ask, and show no tax line unless the company's rules call for one. Keep internal costs separate from customer prices; disclose unit rates where required by the client form.

## Calculation

Use existing trusted calculation facilities (the native draft model where its contract fits, or an inspected Decimal calculation for an agreed draft). Never do the arithmetic yourself, even when it looks easy and even when asked for the total. Without Render or a trusted calculator, show quantities and unit prices, mark line amounts and totals `UNRESOLVED`, and say the calculation capability is missing. Do not create a calculator just to populate this skill. Record exact inputs, formulas, rounding and results. Independently reconcile the output using a different calculation representation or reviewer; repeating the same calculator result is insufficient.

Exclude unselected alternatives from the selected base and label them separately. A required item without a price blocks the full total and any final-price or issuable claim; show any known-items total explicitly as partial. Never drop the mandatory item or price it at zero to pass validation. Native line items have no selection or missing-price flag: filter options explicitly and do not misrepresent a partial native calculation as a complete quotation.

## Check

Check line extensions, selected subtotal, discount, tax basis and rate and total, plus cross-document quantities and scope. A change in option selection or price goes back to step 3 of the skill; layout repairs do not. Keep unresolved decisions visible. Request confirmation does not grant issuance, numbering, signing or sending authority.

Presentation is separate from amounts. Follow `references/document-design.md` to review the surface the reader sees: identifiers, dates, status, payment provenance and long-item pagination. A separately selected company reference pack may inform that review; it does not render the document or approve a financial version. Changing an approved template is a financial revision and needs the required approval.
