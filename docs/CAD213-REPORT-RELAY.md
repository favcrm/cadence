# CAD-213 report relay

`cadence intake` is a small, local relay for projects that explicitly opt in.
It reads local PM issue folders tagged `intake`, publishes a bounded redacted
summary to a configured GitHub repository, and stores receipts, retry timing,
comment cursors, deduplication IDs, claims, and poll health under the runtime
state directory. It does not read the historical inbox or export transcripts.

The relay is disabled when no project is configured, and a project remains
disabled until `--enable` is supplied:

```text
cadence intake configure cadence favcrm/cadence --enable \
  --poll-seconds 300 --actor <github-login>
cadence intake sync --project cadence --once
cadence intake status --project cadence --json
```

`sync` is a cheap non-model poll. It only lists GitHub issues/comments and
updates local state. PM dispatch is a second explicit gate:

```text
cadence intake configure cadence favcrm/cadence --enable --dispatch --pm <pm-alias>
cadence intake sync --project cadence --once --dispatch
```

Dispatch checks the PM agent state and quota telemetry first. Missing or
exhausted telemetry leaves the comment action queued with a durable reason;
the poller does not wake a provider to discover whether quota is available.
Receipt-only comments (`ack`, `/ack`, `received`, and relay markers) are
recorded as seen but do not become PM actions. Actionable comments get a
stable event ID and a persisted claim before `agent_send`; expired claims are
requeued on the next sync.

`intake-relay-state.json` exposes `last_check_at`, `last_success_at`, and
`last_error` for each enabled project. `last_success_at` is only advanced when
the GitHub read/publish/comment pass completed without a transport error; it is
not a synthetic healthy value. Each report receipt separately records
`pending`, `publishing`, `published`, or `retrying`, the GitHub number/URL when
known, attempt count, and the next retry time.

The runtime input contract is the local issue-folder shape from CAD-136/PR73:
an issue carries the `intake` tag and its title/body contains the report. This
branch does not cherry-pick PR73 or depend on its commits at compile time.
After PR73 is merged, its `cadence report` writer must continue to produce that
stable shape; until then the relay can be configured and exercised with the
existing issue writer. `fable-cc` remains an inbox provider and is not treated
as a report listener unless an operator configures and verifies one.

Every published issue carries both a report marker and an exact project marker.
When projects share a GitHub repository, reconciliation and comment ingestion
accept only the matching project marker; label-only, report-only, and foreign
project issues are ignored. This fail-closed boundary keeps a comment from one
project out of another project's durable outbox and optional PM dispatch.

GitHub authentication is delegated to the installed `gh` CLI and its bounded
API calls. Repository values accept only `owner/name`; credentials are never
written to config or included in relay summaries. Development and tests use a
mock transport and do not create external issues. Live enablement and canary
operation require the normal independent QA/Ops gates.
