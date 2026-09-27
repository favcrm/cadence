# Release a reviewed app artifact to Local

A workspace app can produce and review content without a project. A context stores the input defaults for one client or brand. A binding selects the exact connection for a declared app capability. Run approval permits the app's writer and independent reviewer to work; a separate effect approval permits one accepted artifact to leave the app.

The initial release capability is text only. This path does not fetch social content, generate images, publish to Instagram or Facebook, or install the AgenticOS runtime metadata. It delivers a reviewed text item to the registered Local outbox. The Social Content preview remains a fixture until its controls use these APIs.

## App declaration

Declare a provider-neutral need in `app.md`:

```yaml
needs:
  connections: []
  capabilities:
    publication:
      schema: 1
      capability: text.publish
      version: 1
      action: publish
      resource_kind: connection_account
      effect: send
```

The selected workflow declares `publication_slot: publication` in its frontmatter. This declaration selects the slot for the eventual reviewed artifact; it is not a worker tool call. A context default applies only to an input explicitly marked `context_default: true`.

An adapter's reviewed action mapping resolves the capability to its provider tool, required scopes, effect classification and input/output contracts. An unsupported mapping refuses the binding. The app declaration cannot select a provider tool or change the mapping's effect. The new Local `publish_app_text` tool is reserved for the reviewed app-artifact broker; legacy `platform_call` cannot invoke it even with a publish grant.

## Operator flow

Use the stable IDs and digests returned by each command. The examples assume an installed, approved app, a registered team and a completed independently reviewed run.

```sh
cadence connection ls
cadence app binding create INSTALL_ID --context-id CONTEXT_ID \
  --slot publication --connection-id CONNECTION_ID --request-id bind-client-a
cadence app binding show INSTALL_ID BINDING_ID
cadence app run show RUN_ID
cadence app effect stage RUN_ID --artifact-id ACCEPTED_ARTIFACT_ID \
  --slot publication --request-id release-draft-a --title 'Reviewed draft'
cadence app effect show EFFECT_ID
cadence app effect accept EFFECT_ID --digest EXACT_EFFECT_DIGEST
```

Omit `--context-id` when configuring a context-free installation. Bindings must exist when the run freezes its publication intent. An unbound draft cannot inherit a binding added later. Review the complete effect receipt before accepting its digest. Staging reads the stored accepted artifact and its real independent reviewer evidence; it accepts no caller-supplied body, path, provider, grant or verdict.

An operator can decline a staged effect with `cadence app effect decline EFFECT_ID --digest EXACT_EFFECT_DIGEST`. Binding changes use `cadence app binding set INSTALL_ID BINDING_ID --expected-revision REVISION --connection-id CONNECTION_ID`; revocation uses `cadence app binding revoke INSTALL_ID BINDING_ID --expected-revision REVISION`. Revisions prevent a stale editor from overwriting a concurrent change.

## HTTP controls

All routes require a live operator session and proof of the HTTP peer. A stolen operator cookie does not authorize an agent or its detached child. Body schemas reject unknown and duplicate fields, null context selectors and caller identity assertions. Query parameters are unsupported; selection lives in the exact path or typed body.

| Route | Method | Body |
| --- | --- | --- |
| `/api/app-installations/INSTALL_ID/bindings` | GET | None |
| Same route | POST | `slot`, `connection_id`, `request_id`, optional non-null `context_id` |
| `/api/app-installations/INSTALL_ID/bindings/BINDING_ID` | GET | None |
| Same route plus `/update` | POST | `expected_revision`, `connection_id` |
| Same route plus `/revoke` | POST | `expected_revision` |
| `/api/app-installations/INSTALL_ID/contexts/CONTEXT_ID/bindings` | GET | None |
| `/api/app-runs/RUN_ID/effects` | POST | `artifact_id`, `slot`, `request_id`, `title` |
| `/api/app-effects` | GET | None |
| `/api/app-effects/EFFECT_ID` | GET | None |
| Same route plus `/decide` | POST | `digest`, `decision` (`accept` or `decline`) |
| `/api/app-installations/INSTALL_ID/effects` | GET | None |
| `/api/app-installations/INSTALL_ID/contexts/CONTEXT_ID/effects` | GET | None |

The input body is bounded to 48 KiB and successful receipts to 4 MiB. The backend separately bounds the complete canonical tool input and preview. It refuses oversized material instead of truncating what the operator approves. Errors expose a sanitized refusal, not stored content or credentials.

## Release and recovery guarantees

The effect's app-artifact authority records the exact installation, optional context, binding, connection, reviewed provider mapping, immutable run snapshot, accepted artifact bytes and reviewer provenance. Current authority is checked at staging, decision and final execution claim. Each eligible `decided` effect has one checked transition to `executing`; losing that claim makes no adapter call.

The broker serializes authority mutation with the bounded Local commit and releases its SQL connection before the adapter reads its executing permit. The outbox stores an `app_artifact` item with provenance under `app-items/EFFECT_ID`. It does not invent a project, inherit a worker directory or publish attachments. The original project publication tool retains its existing behavior.

An uncertain `executing` effect becomes a reconciliation item after restart. It is not automatically sent again. Operator history retains the original reviewed receipt after context or binding revocation. Retirement of a reviewer does not rewrite a historical accepted review; current installation and release authority still apply.

These checks are broker authority and storage integrity guarantees. They do not claim filesystem confinement against a process sharing the daemon's operating-system user.
