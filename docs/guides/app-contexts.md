# Optional app contexts

A context groups content defaults and run history within one workspace-owned
app installation. It needs no project. Contexts do not grant provider access,
choose a team or approve a run. Existing context-free runs remain supported.

An installed workflow must explicitly declare which content inputs can receive
context defaults, for example:

```yaml
inputs:
  source: { context_default: true, ask: "Paste the source facts" }
  writer: { ask: "Registered writer" }
```

Inputs referenced by worker assignments, structural ticket metadata or the
distinct-worker rule cannot become context defaults. A key must be safe in
every installed workflow that declares it. A run uses only defaults declared
by its selected workflow. Explicit operator run inputs override defaults;
the operator still supplies the team and approves the complete frozen run.

Use a JSON file containing a string map, such as `{"source":"Client facts"}`:

```sh
cadence app context create INSTALL_ID --label "Client A" --defaults defaults.json --request-id client-a
cadence app context ls INSTALL_ID
cadence app context show INSTALL_ID CONTEXT_ID
cadence app context set INSTALL_ID CONTEXT_ID --expected-revision 1 --label "Client A" --defaults revised.json
cadence app context archive INSTALL_ID CONTEXT_ID --expected-revision 2
```

Create permits omitted defaults for an empty map. Set replaces the full label
and defaults and requires an explicit file. Configuration is bounded to 32KiB,
labels to 120 bytes, and inventory to 100 contexts per installation. The commands
require the running daemon and proven operator authority; there is no privileged
offline fallback. IDs belong to the exact installation and are never reused.

Create a draft with the context ID and explicit run inputs:

```sh
cadence app run create INSTALL_ID --context-id CONTEXT_ID --workflow draft --inputs run-inputs.json --request-id draft-a --owner-pm TEAM_PM
cadence app run ls --install-id INSTALL_ID --context-id CONTEXT_ID
```

The immutable run snapshot includes context identity, revision and content
digest. Approval and dispatch remain separate actions. Omitted context means
a context-free run; omitted list filters preserve the operator's full run list.
Workers receive their assigned run's snapshot and can fetch only the exact
dependency artifact of their actual current turn. A shared worker cannot fetch
another context's draft by guessing its ID or reusing its own turn token.

Update and archive require the observed revision. A stale write refuses rather
than overwriting a concurrent change. Changing the context retires unfinished
eligibility and blocks stale dispatch and late results. Historical operator
run and artifact audit remains available. Archival never selects another
context or installation default, and does not automatically resume old runs.

HTTP routes use the same proven operator boundary:

| Method | Route | Body |
|---|---|---|
| GET | `/api/app-installations/:install/contexts` | None |
| POST | `/api/app-installations/:install/contexts` | `label`, `input_defaults`, `request_id` |
| GET | `/api/app-installations/:install/contexts/:context` | None |
| POST | `/api/app-installations/:install/contexts/:context/update` | `expected_revision`, `label`, `input_defaults` |
| POST | `/api/app-installations/:install/contexts/:context/archive` | `expected_revision` |

The path supplies installation/context identity. Unknown fields, unsupported
queries and incorrect types refuse. HTTP bodies are bounded to 48KiB and
context-list receipts to 4MiB. Context selectors on `/api/app-runs` require the
installation selector. Operator cookies do not give managed native peers or
their detached descendants access to these private settings.

This increment stores run-owned text artifacts and independent reviews. It
does not add a generic media library, provider reads, image generation, outbox
release or external publication. Those capabilities require separately reviewed
connection/resource bindings and exact effect approval.
