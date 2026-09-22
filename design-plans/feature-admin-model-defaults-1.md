---
goal: Configure daemon-wide default models by provider and team role through the operator UI
version: 1
date_created: 2026-09-22
last_updated: 2026-09-22
owner: Cadence maintainers
status: Planned
tags: [feature, configuration, models, roles, ui]
---

# Introduction

![Status: Planned](https://img.shields.io/badge/status-Planned-blue)

Add **Settings → Model defaults** to the board. Operators set a baseline model for each supported provider and override it for individual team roles. New agents resolve the configuration once at registration and persist their selected model. Explicit launch settings win, and existing agents retain their saved settings on resume.

This is a study and implementation plan, not an implemented feature. Evidence is from the working tree at commit `5f8f449`; symbol names take precedence over line numbers. The user confirmed team-role scope; daemon-wide scope is the proposed default. No provider model catalog or external service was queried; model IDs below describe contracts rather than recommended products.

### Findings from the current code

| Area | Evidence | Implication |
|---|---|---|
| Registration | `src/daemon.rs:1623`, `rpc_register`, validates launch params and calls `Store::register_agent` before starting an actor | Apply defaults here through the store transaction so CLI and direct RPC launches share behavior |
| Launch and resume | `src/main.rs:5069`, `provider_launch`, builds `params.model`; duplicate registration reuses the existing record | Persist the resolved model; never resolve global defaults during actor open or resume |
| Roles | `src/store.rs:1493` accepts only `pm` and `worker`; `src/daemon.rs:1375`, `memory_actor`, and `src/memory/mod.rs` use these values in identity/authorization checks | Add a separate team-role field; expanding the existing role vocabulary is an authorization change |
| Provider support | `src/adapter/registry.rs`, `SPECS` and `launch_params` | Codex managed/managed-ws, Claude managed/pty, and Cursor pty support model selection; Devin pty and inbox do not |
| Model evidence | `src/store.rs:625`, `Agent::to_json`, separates configured and provider-reported models; `ui/src/components/Agents.tsx`, `profileModel`, displays both | Add configuration provenance without claiming that a requested model was actually used |
| Existing overrides | `src/daemon.rs:2245`, `rpc_set`, and `Store::set_params` implement `agent set --next-launch` | A later explicit override must update or clear inherited provenance atomically |
| Board access | `src/ui.rs`, `write_guard`, `write_route`, `request_actor`, and `ServeOpts.read_only` | The existing board has operator access and optional tailnet attribution, not an admin/user account system |
| Related design | `docs/adr/0001-role-profiles.md` is proposed and describes project `team.yaml`; `docs/TEAM.md` describes team duties | Reuse its role vocabulary; keep this feature independent of profile expansion and capability enforcement |

## 1. Requirements & Constraints

- **REQ-001**: Store one model-defaults configuration per daemon state directory. It applies to all projects and groups served by that daemon. The selected project filter must not alter it. Empty configuration preserves current launch behavior.
- **REQ-002**: Provide a provider baseline plus role overrides. Canonical team-role keys are `pm`, `research`, `architect`, `dev`, `qa`, `devops`, and compatibility fallback `worker`. Display `devops` as “DevOps”; accept `ops` as a launch-input alias and normalize to `devops`.
- **REQ-003**: Keep `agents.role` and `--role` as the current runtime identity values. Add nullable `agents.team_role` and `--team-role` to provider launch verbs, `join`, and low-level agent registration. If absent, use runtime `role` as the model lookup role. Never infer a team role from an agent alias, briefing filename, task role, or upstream. Selecting team role `pm` must not grant runtime PM authority.
- **REQ-004**: For a fresh registration, resolve in this order: explicit non-empty `params.model`; matching provider/team-role setting; provider baseline; provider-native default. A role setting explicitly selecting the provider-native default stops inheritance. A missing role setting inherits the baseline. Custom team roles do not fall back through `worker`; `worker` is only the lookup key for an unclassified runtime worker.
- **REQ-005**: Persist the selected model and provenance at registration. Default edits affect only future registrations. Resume, daemon recovery, duplicate-alias launch, and changing provider-native settings must not re-resolve Cadence defaults for an existing record. Native provider behavior remains outside Cadence's model guarantee when no explicit model is stored.
- **REQ-006**: Derive eligible providers and endpoint kinds from the registry: public entries with `launch_params` containing `model`. Exclude test doubles, retain Devin as a disabled explanatory row, and omit inbox from the model matrix. Baselines are provider-wide across eligible endpoint kinds; unsupported kinds never receive an injected model.
- **REQ-007**: Validate model IDs as trimmed, non-empty strings of at most 200 UTF-8 bytes with no control characters. Treat them as opaque provider IDs; do not invent a shared model-family translation. Reject unknown providers, unsupported settings, unknown roles, duplicate provider/role entries, extra JSON fields, and malformed selector objects. Save uses one atomic revision-checked operation.
- **REQ-008**: Offer free-text model IDs with suggestions from that provider's currently configured and reported agent models. Mark suggestions as previously observed, not a provider catalog. Saving verifies syntax and adapter support; provider availability and account entitlement are checked at launch by the existing provider path. A rejected model must surface the provider error without silently choosing another model.
- **REQ-009**: Show provenance separately from effective provider evidence: explicit launch, role default, provider baseline, or provider-native default; include applied configuration revision where applicable. Existing rows have unknown historical provenance, represented as `legacy_configured` or `legacy_provider_default`.
- **REQ-010**: Settings supports load, edit, save, cancel, per-role reset to inheritance, provider reset to native default, field errors, dirty state, save failure, and revision conflict. On conflict preserve the draft and offer an explicit reload; never retry an overwrite automatically. Disable writes in read-only mode. Show unsupported-daemon and unavailable-daemon states distinctly from an empty saved configuration.
- **SEC-001**: Use existing loopback/tailnet operator access and all existing HTTP write guards. “Admin” means the current operator surface in this phase. No login distinction or exclusive human-admin authorization is implied. Tailnet actor attribution is audit metadata, not an authorization grant. A requirement to distinguish admins from other board users requires a separate access-control design before exposing this feature to those users.
- **SEC-002**: The HTTP process must write through daemon RPC, never directly to SQLite. Record configuration revision, before/after data, timestamp, and transport-derived operator attribution in a daemon event in the same transaction as the configuration update. Direct local RPC uses the existing same-user socket boundary; request-supplied role fields confer no new authority.
- **CON-001**: Scope is model defaults only. Effort remains an explicit existing launch parameter; a model change must still pass existing model/effort validation at open. Provider selection, credentials, permission modes, pricing, budgets, live model switching, bulk agent migration, project overrides, and native subagent models are outside this implementation.
- **CON-002**: Start with empty defaults and nullable new agent metadata. Do not seed models from prose or rename existing runtime roles. Add no runtime dependency. Follow the store's existing schema-migration mechanism and browser request conventions.
- **PAT-001**: If future `team.yaml` expansion supplies an explicit model, it occupies the explicit-launch tier. This plan adds a host-level fallback, not a second project profile authority. Reconcile team-role naming with ADR 0001, documenting that authorization remains on runtime `role` until a dedicated migration.

### Configuration and API contract

Store the document as a singleton SQLite row with `revision INTEGER NOT NULL` and `document TEXT NOT NULL`; initialize revision `0` and `{"schema":1,"providers":{}}`. Revision increments on every successful save, including an identical submitted document. Add `team_role TEXT` and `model_selection TEXT` columns to agents; the latter stores provenance JSON, outside provider launch params.

Example document (the string `MODEL_ID` is an illustrative user-supplied identifier):

```json
{
  "schema": 1,
  "providers": {
    "claude": {
      "default": { "mode": "model", "model": "MODEL_ID" },
      "roles": {
        "qa": { "mode": "provider_default" }
      }
    }
  }
}
```

Each provider entry requires `default` and `roles`. `default` and role selectors are either `{mode:"model", model:string}` or `{mode:"provider_default"}`. Remove a role key to inherit; remove a provider key to reset that entire provider. These operations are distinct from an explicit role-level native-default selector. The UI exposes all three role choices: “Inherit provider baseline”, “Provider-native default”, “Specific model”.

| Interface | Contract |
|---|---|
| `model_defaults_get` RPC / `GET /api/settings/model-defaults` | Return `{revision, config, providers, roles}`. Provider descriptors contain eligible kinds and model suggestions. Derive capabilities server-side; GET must not start provider processes. HTTP also reports `read_only` from board options. |
| `model_defaults_set` RPC / `POST /api/settings/model-defaults` | Request `{expected_revision, config}`; require a nonnegative integer revision and a document no larger than 16 KiB. HTTP body cap is 20 KiB. Atomically replace the document and append an audit event. Return the same fresh snapshot shape as GET. |
| Error mapping | HTTP 400 invalid data, 403 write guard/read-only refusal, 409 revision mismatch, 501 older daemon without the RPC, 503 daemon unavailable. Return stable error codes; conflict includes current revision. Use structured RPC errors, not message substring matching. |
| New registration fields | Top-level `team_role?: string`, `model_policy?: "inherit" or "provider_default"`; omission means inherit. Add mutually exclusive CLI `--model` and `--provider-default-model` where model selection exists. The latter passes `model_policy:"provider_default"`, bypassing both default tiers. Neither field is forwarded to provider argv. |
| Provenance | `model_selection` contains `source`, `lookup_role`, `revision`, and `model`; nullable revision for explicit/legacy values. Role-native selection records role-default source with a null model. Also expose the nullable `team_role` and the actual lookup role in agent JSON. Preserve the existing `model_source`, `model_configured`, and `model_reported` contracts. |

The settings layout uses a provider section with baseline selector and a role table below it; on narrow screens roles become labeled rows. Display “Applies to new agents across all projects” next to the page description. Keep Save/Cancel adjacent to the edited configuration. The Agents view shows team role beside runtime role where present and the applied model source beside the existing configured/reported distinction.

Provenance `source` values are `explicit`, `explicit_provider_default`, `role_default`, `provider_baseline`, `provider_default`, `legacy_configured`, and `legacy_provider_default`. Use the applied snapshot revision for inherited decisions, including an empty configuration; use null for explicit and legacy decisions. Do not accept client-supplied provenance. Unsupported endpoints have null model-selection provenance and never receive an injected model.

## 2. Implementation Steps

### Implementation Phase 1

- **GOAL-001**: Define the configuration schema and durable storage. Complete when migrations preserve existing rows, selector validation passes, and concurrent saves cannot lose edits. Tasks are sequential in listed order.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-001 | Add `src/model_defaults.rs` and export it from `src/lib.rs`. Implement `ModelDefaults`, `ModelSelector`, `ModelSelection`, `normalize_team_role`, `validate_config`, and pure `resolve_model`. Encode REQ-002 through REQ-008 and the exact JSON contract above. Use tagged serde enums and deny unknown fields. Detect duplicate map keys during deserialization rather than allowing last-key-wins parsing. Add registry-derived `supports_model` and public provider-descriptor helpers in `src/adapter/registry.rs`; explicitly exclude fake/internal endpoints. | Yes | 2026-09-22 |
| TASK-002 | Extend `src/store.rs`, `Store::open_inner`, with the singleton `model_defaults` table and nullable agent columns. Extend `Agent`, `NewAgent`, `row_agent`, and construction fixtures for the fields. Add `Store::model_defaults` and `Store::replace_model_defaults` using one write transaction for revision comparison, update, and `model_defaults_updated` event on the daemon event stream. Return a typed revision conflict without partial writes. | Yes | 2026-09-22 |
| TASK-003 | Extend `Store::register_agent` so defaults snapshot, resolution, selected `params.model`, provenance, and insertion share one transaction. Validate supplied and resolved params through registry rules; apply the existing 4000-character stored-params cap after merging. Explicit null/blank/non-string models reject rather than becoming implicit inheritance. Unsupported endpoints preserve current behavior without injecting settings. Legacy records receive no model rewrite. Ensure duplicate-alias handling remains detectable and does not alter existing agents. | Yes | 2026-09-22 |

### Implementation Phase 2

- **GOAL-002**: Make every fresh launch consume the defaults, and expose configuration through the daemon and guarded HTTP API. Depends on Phase 1; complete when CLI/RPC parity, explicit overrides, resume stability, and HTTP contracts pass. Tasks are sequential.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-004 | Update `src/daemon.rs`, `rpc_register`, to parse and validate top-level team role/model policy and pass them to the store. Add `model_defaults_get` and `model_defaults_set` dispatch handlers. Extend `src/proto.rs`/`src/error.rs` only as needed for typed conflict/unsupported-method mapping. Derive audit attribution using the existing transport trust boundary and label local RPC separately from board attribution. | Yes | 2026-09-22 |
| TASK-005 | Update provider launch command variants, `Join`, low-level registration, and `provider_launch` in `src/main.rs` with `--team-role` and eligible-provider `--provider-default-model`; reject conflicts with `--model` before registration. Normalize `ops` to `devops`. Preserve all existing defaults for runtime role and existing launch behavior when no configuration exists. Team-role configuration must not change runtime-role permissions. | Yes | 2026-09-22 |
| TASK-006 | Update `Store::set_params` and `src/daemon.rs`, `rpc_set`, so a successful explicit next-launch model change updates model provenance in the same transaction. Clearing model restores provider-native behavior for that saved agent and records explicit-provider-default provenance; it does not opt back into global inheritance. Failed validation changes neither params nor metadata. Reuse of an existing alias must resume/reuse saved settings even if a newer global default would fail validation. | Yes | 2026-09-22 |
| TASK-007 | Add exact GET/POST settings routes in `src/ui.rs`. Perform route/method, read-only, origin/header/content-type checks before reading the bounded body. Forward to daemon RPC, map structured failures to the contract, and derive operator attribution via `request_actor`. Preserve original JSON until duplicate-key validation has occurred. Add a daemon capability flag so newer boards can explain older-daemon incompatibility. Extend agents list/detail payloads with team role and model provenance. | Yes | 2026-09-22 |

### Implementation Phase 3

- **GOAL-003**: Deliver a responsive settings page with understandable inheritance and model evidence. Depends on Phase 2; complete when configuration round-trips through the browser, conflict drafts survive, and read-only/unsupported states work. Tasks are sequential.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-008 | Add model-defaults/provenance types in `ui/src/types.ts` and dedicated typed GET/POST methods in `ui/src/api.ts`. Preserve existing issue-write response typing. Add `settings` to `ui/src/urlState.ts`, desktop `Sidebar.tsx`, and mobile navigation and rendering in `App.tsx`. Make Settings independent of selected project and reachable at `?tab=settings`. | Yes | 2026-09-22 |
| TASK-009 | Add `ui/src/components/ModelDefaults.tsx`. Implement provider baseline, role overrides, suggestions, draft validation, dirty tracking, Save/Cancel, per-role inheritance reset, provider-native selection, and provider reset under REQ-010. Wait for metadata/config before enabling edits. Freeze form edits during save; preserve draft on errors. A conflict requires explicit reload and shows that this discards the draft. Refresh on page entry without replacing a dirty draft during background updates. Use labeled controls and existing styling; avoid adding a UI library. | Yes | 2026-09-22 |
| TASK-010 | Extend `ui/src/components/Agents.tsx`, `profileModel` and profile/details rendering, to show team role, source, and applied revision without substituting configured values for confirmed effective values. Preserve graceful fallback for older payloads and unknown legacy provenance. Display unsupported model selection for Devin and keep provider-native effective-model uncertainty explicit. | Yes | 2026-09-22 |

### Implementation Phase 4

- **GOAL-004**: Verify the complete user outcome and document operational semantics. Depends on Phases 1–3; complete when targeted Rust/UI checks pass and browser evidence covers the scenarios below. Tests and documentation can proceed independently after implementation.

| Task | Description | Completed | Date |
|------|-------------|-----------|------|
| TASK-011 | Add meaningful resolver/store tests in `src/model_defaults.rs` and `src/store.rs`; integration tests in `tests/integration.rs`; guarded HTTP tests in `tests/board.rs`; settings URL tests in `ui/src/urlState.test.ts`. Use isolated state directories and existing provider mocks; no paid model calls. Cover TEST-001 through TEST-007. | Yes | 2026-09-22 |
| TASK-012 | Run targeted suites with the repository's `scripts/cadence-nextest` runner after checking its documented invocation; run `pnpm -C ui typecheck`, `pnpm -C ui test:url-state`, and `pnpm -C ui build`. Exercise TEST-008 against a disposable daemon/UI and record screenshots plus request outcomes. Consult the applicable frontend/browser skills when implementing and testing the UI. | Yes | 2026-09-22 |
| TASK-013 | Update `docs/PROTOCOL.md` with RPC fields, capabilities and error codes; update `docs/TEAM.md` with `--team-role`, default precedence, scope and resume behavior. Add a dated note to `docs/adr/0001-role-profiles.md` distinguishing this team-role metadata from its proposed future authorization/profile migration. Document recovery through per-role/provider reset and existing explicit next-launch overrides. | Yes | 2026-09-22 |

## 3. Alternatives

- **ALT-001**: Only support runtime `pm`/`worker`. Smaller implementation, but cannot distinguish developer, QA, research, architecture and operations model choices. Use only if the operator explicitly chooses runtime-role scope.
- **ALT-002**: Expand `agents.role` directly. Aligns with a future role-profile migration but currently changes assumptions in memory proof and receipt validation. A separate team-role field keeps model preferences independent of authority.
- **ALT-003**: Store defaults in `ui.json` or browser local storage. Rejected because direct CLI/RPC launches need the same durable source and UI processes must not own daemon launch policy.
- **ALT-004**: Implement complete project `team.yaml` editing first. Adds project ownership, profile expansion, and permission concerns beyond this request. Future explicit profile values can override daemon baselines using the stated precedence.
- **ALT-005**: Resolve on every resume or automatically update running agents. Rejected because a defaults edit would unexpectedly change existing sessions and model/effort compatibility.
- **ALT-006**: Build live provider catalogs and a model recommendation service. Deferred because the current registry contains launch capabilities, not a unified model catalog. Free text plus clearly labeled observed suggestions works without provider process startup or stale hardcoded model lists.

## 4. Dependencies

- **DEP-001**: Existing serde/serde_json/rusqlite migration and transaction facilities; no additional runtime package is required.
- **DEP-002**: Registry launch capability and model validation functions, existing provider-open validation, and provider mocks in the integration suite.
- **DEP-003**: Existing operator board boundary, read-only mode, daemon RPC transport, and daemon event stream. A distinct multi-user admin permission system is not present and is not provided by this plan.
- **DEP-004**: Implementation phases have the explicit dependencies described under each phase; the proposed full role-profile ADR is contextual, not a prerequisite.

## 5. Files

- **FILE-001**: `src/model_defaults.rs` (new), `src/lib.rs`: schema, role normalization, selector validation, pure resolution and exports.
- **FILE-002**: `src/store.rs`: migration, transactional config storage, registration snapshot, team-role/provenance fields and override updates.
- **FILE-003**: `src/adapter/registry.rs`: capability-derived eligible providers and model-setting validation helpers.
- **FILE-004**: `src/daemon.rs`, `src/main.rs`, `src/proto.rs`, `src/error.rs`: registration inputs, settings RPC, CLI flags and structured errors.
- **FILE-005**: `src/ui.rs`: guarded HTTP settings routes and additional agent metadata.
- **FILE-006**: `ui/src/components/ModelDefaults.tsx` (new), `ui/src/components/Agents.tsx`, `ui/src/components/Sidebar.tsx`, `ui/src/App.tsx`, `ui/src/urlState.ts`, `ui/src/api.ts`, `ui/src/types.ts`: settings UI, navigation, model source display and API types. Extend `ui/src/styles.css` only if existing classes cannot express the required responsive layout.
- **FILE-007**: `tests/integration.rs`, `tests/board.rs`, `ui/src/urlState.test.ts` and unit tests beside resolver/store code: automated behavioral coverage.
- **FILE-008**: `docs/PROTOCOL.md`, `docs/TEAM.md`, `docs/adr/0001-role-profiles.md`: public contracts and role-profile relationship.

## 6. Testing

- **TEST-001**: Table-driven precedence for explicit model, role model, role-native selector, baseline model, baseline-native selector, omitted config, unclassified worker, team-role alias normalization and explicit provider-native bypass. Verify no cross-provider or unrelated-role fallback.
- **TEST-002**: Invalid JSON fields, duplicate keys, bad roles/providers, unsupported endpoint settings, control characters, byte limits, whitespace-only values, conflicting explicit model/policy and non-string model values reject with zero writes. Validate final merged params size.
- **TEST-003**: Migration preserves old agent params and memory identity fields. Two clients saving revision N yield exactly one success and one conflict; restart retains the successful snapshot. Audit event and config commit or roll back together. Registration concurrent with save observes one complete revision and matching provenance.
- **TEST-004**: CLI join/provider launch and direct RPC registration receive the same inherited model using existing mock commands. Explicit models win. A team-role `qa` runtime worker remains a worker for memory authorization; a team-role `pm` runtime worker cannot finalize memory. Existing PM access remains unchanged.
- **TEST-005**: Save defaults A, register an agent, save B, resume and recover the original agent: its stored A remains. Register a fresh agent: it receives B. Duplicate launch reuses saved configuration even when the new default is invalid at provider open. Explicit next-launch set/clear updates provenance atomically and clearing never reapplies B.
- **TEST-006**: Supported adapter mocks receive the selected model in their existing request/argv shape; Devin/inbox receive no injected model. A mock provider rejection or model/effort incompatibility surfaces an error rather than a fallback. Configured and provider-reported model mismatches remain visible.
- **TEST-007**: HTTP GET/POST round-trip, read-only refusal, wrong Host/Origin/content type/custom header, oversized body, unknown method/path, conflict, missing daemon and unsupported older daemon. Rejected requests do not mutate configuration. Reloaded settings and agent APIs preserve team-role/provenance fields.
- **TEST-008**: Browser at desktop and 390 px width: navigate Settings, set baseline and QA override, choose role-native default, reset to inheritance, save and reload; verify cancel, invalid input, two-tab revision conflict with retained draft, failed save, read-only and unsupported daemon. Confirm keyboard labels and project-independent scope. Launch a mock agent and inspect requested versus reported model in Agents. Record evidence without calling live paid providers.

## 7. Risks & Assumptions

- **RISK-001**: The word “admin” may imply distinct accounts. The current application only has operator access. This draft adopts that existing boundary; it must not advertise exclusive admin permissions.
- **RISK-002**: Team roles and runtime roles can be confused. Label both explicitly, normalize the documented ops alias, preserve runtime authorization checks and explain the future migration relationship. Existing agents stay unclassified until deliberately recreated with a team role; alias names are insufficient evidence.
- **RISK-003**: A provider may reject an ID accepted syntactically at save, or a new model may reject an explicitly supplied effort. Keep validation-stage messaging honest and preserve provider launch errors. Do not hardcode supposedly current catalogs.
- **RISK-004**: New board and old daemon versions can coexist. Capability/error handling must show unavailable functionality without silently writing through another path or displaying an empty configuration as a successful read.
- **RISK-005**: Adding provenance only during launch leaves stale labels after explicit overrides. Registration and `set_params` must maintain params/provenance transactionally; legacy provenance must remain explicitly unknown.
- **ASSUMPTION-001**: The user confirmed team roles: PM, developer, QA, researcher, architect and DevOps. The plan retains `worker` only as a compatibility lookup for launches without a team role; runtime PM/worker-only scope is an unselected alternative.
- **ASSUMPTION-002**: Defaults are daemon-wide, not project-specific; they choose a model within the already-selected provider. They do not select a provider for a role.
- **ASSUMPTION-003**: The requested deliverable is a studied implementation plan. Runtime implementation, deployment, issue creation and agent messaging have not been performed.

## 8. Related Specifications / Further Reading

- [Role profiles ADR](../docs/adr/0001-role-profiles.md): proposed team configuration and future role authority migration.
- [Team operating model](../docs/TEAM.md): role duties and current model/effort guidance.
- [Protocol](../docs/PROTOCOL.md): daemon request and agent-setting contracts.
- [Architecture](../docs/ARCHITECTURE.md): daemon, adapter and storage boundaries.
- [Provider registry](../src/adapter/registry.rs): actual endpoint model support; implementation truth for the UI matrix.
- [Existing agent model display](../ui/src/components/Agents.tsx): configured versus reported model presentation to preserve.


## Delivery notes (2026-09-22)

Implemented on branch `cadence/admin-model-defaults`. Material adjustments:

- Legacy provenance is derived when an agent row is read. Existing `model_selection` values are not backfilled, so resume does not rewrite saved params.
- `agent_identity` does not include `team_role` or `model_selection`. Queued-message identity comparison stays stable.
- Unsupported-daemon detection uses the `model_defaults` health capability. The board does not match unknown-method error text. The 501 decision is unit-tested against a capability list; the disposable daemon used in HTTP tests already has the capability.
- An oversized settings body returns HTTP 400 `invalid_request` (the invalid-data class) rather than 413.
- Fake and internal endpoints never receive an injected model. `ops` is normalized only as launch input and is not a stored config role key.
- HTTP writes go through daemon RPC. The raw document string is forwarded so duplicate JSON keys are still rejected.
- The settings editor compares documents with sorted object keys. The daemon's `serde_json::Value` map is a `BTreeMap`, so a clean revision 0 and a just-saved revision otherwise stringify as unsaved (`providers` before `schema`).
