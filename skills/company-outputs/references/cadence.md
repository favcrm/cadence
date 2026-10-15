# Supported Cadence path and limits

Source contracts inspected in the CAD-1451 lane: `src/cli/business_templates.rs`, `src/business_templates.rs`, `src/daemon/business_templates_rpc.rs`, `src/store/business_templates.rs`, `src/business_export.rs` and the existing `skills/business-documents/` workflow. Installed `cadence template`, `template create`, `template sample` and `document` help were also inspected. Recheck current contracts for another version; this is not proof of hosted access or successful rendering.

## Native operations

- `cadence template intake FILE [--out DIRECTORY]` reads DOCX/readable PDF reference material. A PDF reconstruction is editable but needs separate approval.
- `cadence template create NAME --docx FILE --file SPEC.json` registers a draft version; `revise NAME --base VERSION --docx FILE --file SPEC.json` creates the next version. Neither approves it. Use only when registration is authorised.
- `cadence template show NAME --version VERSION` supplies the digest. `template sample NAME --version VERSION --out DIRECTORY` produces a labelled placeholder SAMPLE, never a real-data draft. Calculation rules must already be confirmed even for SAMPLE. This creates output files, not a retained business-document record.
- Native approval is the operator-only `template approve NAME --version VERSION --sha256 DIGEST --source SOURCE`. The agent does not impersonate the operator or record a simulated approval.
- Real-data drafting exists: `cadence document create NAME --version VERSION --file DATA.json`, then `document show ID` and `document export ID --out DIRECTORY`. Create requires an approved exact template version and **persists a retained draft with input/context and template digest**; there is no stateless real-data preview command. Export writes both DOCX and PDF, without sending, issuing or allocating an invoice number. SAMPLE also writes both formats. There is no DOCX-only export flag. If the request prohibits PDF generation, report this incompatibility rather than invoking it or adding a converter. The user handles final PDF export.

Company scope and caller authority come from the authenticated connection, not request fields. Do not switch orgs, change identity, wake/restart a daemon, edit databases or approve templates to work around access failures. An inaccessible native path requires a precise result and explicit agreement before using a local alternative.

## Data and calculations

Native `kind` is `quotation` or `invoice`; there is no proposal/report/letter kind. A long proposal must not be disguised as a quotation. The spec has `fields` (key, label, kind text/date/money/number, required), `fixed_text` and `calculation` (confirmed, currency, tax_rate_bps, discount_bps). Native data has `fields` and `items`; each item has description, quantity and unit_price as decimal strings. Unknown fields are refused. See the current source for bounds; text fields are limited to 2,000 bytes and at least one line item is required.

Amounts have two decimal minor digits; positive quantities have up to three decimals. Nonnegative prices and tax/discount rates of 0..10000 basis points are accepted. Integer arithmetic rounds half-up per line, then document discount, then tax on subtotal less discount. This is not configurable currency precision or line-level tax. Currency syntax is three uppercase ASCII letters, not validated ISO membership. Do not impose these semantics on a company's incompatible calculation rules. Missing required fields or submitted items with unparseable/missing prices are refused, but the model cannot detect a required item omitted entirely. Options and mandatory-unpriced status remain authoring/coverage responsibilities.

## Existing local tools

Reuse the inspected `skills/business-documents/scripts/run_isolated.py` wrapper only when its operator guide and permissions fit. It requires Docker access and the exact offline image `cadence-business-files:cad-1393-dev1`, ID `sha256:06ea75fdd2b41762d37dcf68a101703be3962976e6be1aebd2247503431cee64`, with its required workflow label. Output must be an existing separate directory owned/writable by the caller; root UID/GID is refused. Never call internal parsers/renderers outside the wrapper, weaken its pin, rebuild/relabel an image to bypass it, or add script hooks. The general render path also generates PDF; its financial XLSX is a fixed Suggested quotation, not arbitrary company-form filling. Missing prerequisites are reported limitations, not reasons to substitute another renderer silently.

## Consumption

This repository keeps auxiliary skills as directories under `skills/`, consumed by providing the checked-out entry point to an authorised worker (as documented in `business-documents/OPERATOR.md`). `cadence skill install` embeds/installs only the Cadence protocol; it does not install this directory. A portable archive can contain `company-outputs/SKILL.md` and its references, excluding company data and trial artifacts. Local unpack/read validation demonstrates packaging and consumption only, not hosted installation or automatic runtime discovery.

## Native acceptance boundary

For a fresh real-data native trial, establish authorised access, an already approved exact template, agreed calculation rules and permission to retain the synthetic draft and generate both export formats. Do not register/approve a template, create a draft or export PDF merely to obtain a passing demonstration. An existing retained draft may be inspected/exported only with access and permission, and it does not prove execution of new trial data.

Proposal kind and arbitrary XLSX-template filling are unsupported here. Quotation execution is implemented but not established by CLI help, source inspection, a SAMPLE, a registry digest or local authoring. Report a precise unsupported/inaccessible/format-conflict result when prerequisites are absent. Keep portable skill acceptance separate from native execution and hosted installation.
