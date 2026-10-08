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
historical runs; it never creates a project. In the board, open
`/app-installations/<id>` → **Settings** → **App package**, enter the source
path or Git URL, select **Check update**, inspect the proposed version, digest
and changed files, then select **Apply checked update**. The board sends the
same pinned arguments as the CLI; a stale proposal is refused. This requires a
verified operator in a writable (not read-only) session. The `/apps` home may
also offer the operator **Review update** when a prior update check found a
changed digest.

Installing or updating is the operator's consent for the exact installed
digest when the bundle passes local execution validation. `cadence app catalog
approve <install-id> --digest <digest>` explicitly approves the exact current
digest for supported local artifact steps; `revoke` with the same arguments
withdraws local capabilities for that digest and does not change legacy
project approvals. Either command refuses a stale digest. Approval of an app
bundle does not approve an individual run or authorize outward effects.

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
state, or an unresolved Local effect in `waiting`, `decided`, `executing` or `reconcile`,
cannot be upgraded. Finish or explicitly cancel the run and decide or
reconcile the effect first. The
upgrade snapshots and validates the new bundle before writing a journal. The old bundle remains at its original path; new bundle
bytes are written beneath a digest-named `revisions/` directory. The output
reports changed, added, and removed files; incompatible context defaults; and
bindings that require an explicit new-version binding. Existing context and
binding rows are retained. A context whose defaults are incompatible with the new bundle
cannot authorize a new run until its defaults are updated. Binding receipts
pin the old bundle and cannot silently grant a new run authority. A second
configured binding for the new digest may use the same slot and context;
updating an old-version binding into a new digest is refused. A successful
operator update records consent for the new digest when it passes the local
execution checks. If local capabilities were later revoked, use the
digest-pinned `cadence app catalog approve` command above to restore them after
reviewing the bundle. Approval still validates the local execution contract; it
cannot bypass a failed check. Run approval and outward-effect permissions remain
separate.

A completed, reviewed draft may still be released to Local from its exact
retained old bundle, and a retained Instagram source receipt may be selected
in a new-version post without another provider read. Both paths recheck the
original approved epoch and immutable run/receipt, the live context, and the
old-version binding's current connection and scopes. An explicit binding or
installation capability revoke stops that historical use. Active runs and
worker calls never inherit the old epoch.

This code advances the daemon SQLite schema from v27 to v28 to retain approval
epochs and index configured bindings by bundle digest. A production daemon
upgrade therefore needs the separate Cadence rollout lease and backup receipt
for that schema crossing; a package-only app upgrade does not migrate the
daemon database.

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

Built-in app bundles and their catalog index are embedded in the Cadence
binary. A new version of a built-in app arrives with a new Cadence build. The
Explorer's operator-only `update_available` flag means a check for the installed
app found a different bundle digest and cached that result; members do not see
the flag. The remote CLI (`cadence --org …`) has no app management verbs: in a
hosted AgenticOS container, install and update apps from the hosted board as the
operator.

This transport currently handles the package bundle and Cadence-owned app
metadata. Host-managed per-install SQLite storage and staged schema/data
migrations belong to the separate App storage contract; they must be integrated
before an app upgrade that changes its data schema is allowed.
