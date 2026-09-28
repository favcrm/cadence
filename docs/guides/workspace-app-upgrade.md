# Upgrade a workspace app

Inspect the installed row with `cadence app catalog show <install-id>`. Record its
`digest` and `catalog_generation`, then check the complete new source bundle.
The read-only check returns the exact proposed `digest` and a file-level
structural diff:

```sh
cadence app catalog upgrade-check <install-id> <absolute-source-path-or-git-url> \
  --expected-digest 'sha256:…' \
  --expected-generation '<catalog-generation>'
```

An upgrade keeps the installation ID, contexts, bindings, and
historical runs; it never creates a project.

```sh
cadence app catalog upgrade <install-id> <absolute-source-path-or-git-url> \
  --expected-digest 'sha256:…' \
  --expected-generation '<catalog-generation>' \
  --expected-new-digest '<digest-from-upgrade-check>' \
  --request-id '<unique-request-id>'
```

The operator must call this from an operator connection. The same operation is
available to an authenticated operator through `POST
/api/app-installations/<install-id>/upgrade`. The read-only preview is `POST
/api/app-installations/<install-id>/upgrade/check`; it accepts `source`,
`expected_digest`, and `expected_generation`. Apply also requires
`expected_new_digest` and `request_id`. Identity, approval,
and actor fields are never accepted from the caller. A repeat of the exact
request returns the committed result; a changed proposed bundle, stale digest, stale generation, or
reused request ID with different material is refused.

An installation with a run in `awaiting_approval`, `approved`, or `running`
state cannot be upgraded. Finish or explicitly cancel that run first. The
upgrade snapshots and validates the new bundle before writing a journal. The old bundle remains at its original path; new bundle
bytes are written beneath a digest-named `revisions/` directory. The output
reports changed, added, and removed files; incompatible context defaults; and
bindings that require an explicit rebind. Existing context and binding rows
are retained. A context whose defaults are incompatible with the new bundle
cannot authorize a new run until its defaults are updated. Binding receipts
pin the old bundle and cannot silently grant a new run authority. The new
bundle digest is unapproved, so approve it separately before creating new runs.

If Git delivery fails after staging, catalog reads and runtime admission stay
closed while `.apps/upgrade-pending.yaml` exists. The error prints the exact
recovery command:

```sh
cadence app catalog upgrade-recover <install-id> --request-id '<request-id>'
```

Recovery validates the retained journal, old bundle, installation identity,
and new bundle before resuming delivery. It does not replay a provider call.
There is no in-place rollback after a committed upgrade. To return to earlier
bytes, submit a new upgrade from those bytes with the current digest and
generation, then review and approve that digest again.

This transport currently handles the package bundle and Cadence-owned app
metadata. Host-managed per-install SQLite storage and staged schema/data
migrations belong to the separate App storage contract; they must be integrated
before an app upgrade that changes its data schema is allowed.
