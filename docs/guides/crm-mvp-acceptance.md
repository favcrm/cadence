# CRM MVP acceptance matrix

CAD-865's acceptance matrix for the real installable CRM — one row per
promised MVP flow, each mapped to its source and the evidence that
proves it. Rows marked **missing** name the verified gap; rows marked
**browser-pending** have daemon/HTTP and mounted-UI proof but await the
real-browser run this lane owns.

**Source SHA basis:** `d379ae54` (`git rev-parse HEAD` on the lane
worktree). Historical CAD-786 browser evidence lives under
`/home/ubuntu/.cache/fb597/e2e` at its own SHA and is retained as prior
evidence — it does not validate this head.

## How to read

- **Component** — the promised flow.
- **Source** — the code path that owns it (files under
  `ui/src/features/app-shell/`, `src/`, `workspace-apps/crm/`).
- **Existing evidence** — the test or harness that proves the behaviour
  today.
- **Verdict** — `pass` (proven at this SHA), `browser-pending` (proven
  in harness/mounted tests, awaiting the real-browser run), `missing`
  (a verified gap this lane found), `fixed` (a gap this lane closed).
- **Fresh browser evidence** — what the owned browser run must still
  demonstrate.

## Matrix

| Component | Source | Existing evidence | Verdict | Fresh browser evidence |
|---|---|---|---|---|
| Package install + digest approve | `workspace-apps/crm/app.md`, `app_workspace_install`, `app_local_install_approve` | `tests/crm_package.rs` (installs, approves, upgrades) | pass | board lists the installation |
| Customer list/search/detail | `CrmCustomers.tsx`, `hostActions`, `src/ui/app_records.rs` | `ui/tests/crmCustomers.test.ts` (mounted), `tests/crm_customers_csv.rs` | browser-pending | list → search → open drawer at desktop + narrow |
| Customer create/edit | `CrmCustomers.tsx` `CustomerNew`/`CustomerDrawer`, `app_record_create/update` | `ui/tests/crmCustomers.test.ts` (create lands on drawer; stale edit refuses) | browser-pending | New customer → fill → drawer opens; stale edit shows refusal |
| **CSV preview/import** | `CustomerCsvImport.tsx` + `csvClient.ts` (new); daemon `app_record_csv_preview/import`; HTTP `records/csv-preview\|csv-import` | `tests/crm_customers_csv.rs` (full RPC/HTTP/CLI); `ui/tests/crmCustomers.test.ts` + `ui/tests/crmCsv.test.ts` (new) | **fixed** — was missing: `CrmCustomers.tsx:274` said "import a CSV once that flow lands" | paste CSV → Preview plan → confirm revision → Import → receipt rows → Return re-reads list |
| Segment create/detail/audience preview | `CrmSegments.tsx`, `audienceClient`, `segmentGrammar`, `app_audience_*` | `tests/app_audiences*.rs`, `ui/tests/crmSegments.test.ts` (mounted) | browser-pending | New segment → rule → drawer with live counts |
| Segment edit/CAS | `CrmSegments.tsx` edit drawer | `ui/tests/crmSegments.test.ts` (stale refuses) | browser-pending | edit rules against stale revision shows refusal |
| Campaign create | `CrmCampaigns.tsx` `CampaignNew`, `contentClient.save` | `ui/tests/crmCampaigns.test.ts` | browser-pending | New campaign → audience radio + preview counts live |
| Campaign content edit/approve | `CrmCampaigns.tsx` `CampaignWorkspace`, `app_content_save/approve` | `ui/tests/crmCampaigns.test.ts` | browser-pending | save subject/blocks → r1 → approve content-only |
| Visual composer blocks | `CrmCampaigns.tsx` block editor, `campaignGrammar` | `ui/tests/crmCampaigns.test.ts` | browser-pending | add heading/paragraph/button blocks |
| Host-rendered preview | `CrmCampaigns.tsx` preview panel, `app_content_render` | `ui/tests/crmCampaigns.test.ts` (iframe + text tab) | browser-pending | Render preview → visual iframe + Text tab |
| Assistant Apply/Discard | `CrmCampaigns.tsx` proposals, CAD-813 mint | `tests/cad813_verified_assistant*.rs`, `ui/tests/crmCampaigns.test.ts` | browser-pending (assistant turn needs a live worker — evidence covers mint control + operator-submitted proposals) | mint disabled without scoped chat; proposal applies as new revision invalidating approval |
| SMTP bind | `CrmCampaigns.tsx` sender panel, `crm_smtp_bind`, `connection_create` | `tests/crm_smtp*.rs` | browser-pending | bound sender panel shows link/auth revisions |
| Test send (real SMTP, 1 recipient) | `sendClient.smtpTestSend`, `crm_smtp_test_send`, CAD-785 rig | `tests/crm_smtp_send.rs`, `tests/crm_send.rs` | browser-pending | test send to operator address → "Accepted … not proof of inbox delivery" |
| Audience freeze | `audienceClient.prepare`, `app_audience_prepare` | `tests/crm_send.rs`, `ui/tests/crmCampaigns.test.ts` | browser-pending | freeze under operator-named ID → count receipt |
| Prepared send + typed-count approve | `sendClient.sendPrepare/Approve`, `crm_send_prepare/approve` | `tests/crm_send.rs`, `ui/tests/crmSend.test.ts` | browser-pending | prepare shows exact counts → typed-count confirm gates approve |
| Bounded send + durable outcomes | `crm_send_rpc`, `sendClient.sendShow/List` | `tests/crm_send.rs` (accepted/failed/uncertain/resolve) | browser-pending | progress → terminal state, per-row outcomes |
| Unsubscribe suppression | `src/ui/crm_send.rs` GET page + POST redeem | `tests/crm_send.rs::cad786_unsubscribe_suppresses_the_recipient` | browser-pending | open minted `/unsubscribe/<token>` → POST suppresses |
| Reload/deep links | `?ctx=&crm=&record=` route state | `ui/tests/crm*.test.ts` (mounted) | browser-pending | reload on detail URL restores drawer |
| Empty/error/stale/read-only | per-screen `data-state`/`role=alert` states | `ui/tests/crm*.test.ts` | browser-pending | empty lists, error retry, read-only renders no forms |
| Persistent single chat | `AppShell.tsx` master thread | `ui/tests/appShell.test.ts` (CAD-863 adjacent) | browser-pending | chat stays mounted across CRM section moves |

## Verified defects this lane closed

- **CSV import had no UI** — `CrmCustomers.tsx` told operators to
  "import a CSV once that flow lands" while the daemon, HTTP peer and
  CLI all served the verbs. This lane added `CustomerCsvImport.tsx` +
  `csvClient.ts` (preview → per-row decisions → token-bound import →
  durable receipt) wired into `CrmCustomers.tsx`. Verdict: **fixed**,
  pending browser proof.

## Verified defects still open (need PM allocation)

- None beyond the CSV flow — every other promised row has source and
  test evidence. The acceptance run itself is the remaining gap, not a
  product defect.

## Browser-run prerequisites (blocked at this writing)

The real-browser run needs a **test-seam build** of `cadence` (the
hold-mode harness `crm_send_e2e_harness` is `#[ignore]`d and gated on
`cfg(feature = "test-seam")`, and `cadence ui login` mints a session
only through the seam). Building that needs a Cargo slot; the caller's
`build-slot status` is refused (unregistered pane). The PM owns either
an admitted `build.recipes` entry for `cargo test --features test-seam
--test crm_send_e2e_harness` plus `cargo build --features test-seam`, or
CI coverage of the same. The owned local headless Chrome session and
`/tmp/c865-0930` workspace are ready; the missing piece is the seam
binary and board. Until then no fresh-browser evidence is claimed.
