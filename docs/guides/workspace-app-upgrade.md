# Upgrade a workspace app

Inspect the installed row with `cadence app catalog show <install-id>`. Record its
`digest` and `catalog_generation`, then check the complete new source bundle.
The read-only check returns the exact proposed `digest` and a file-level
structural diff:

```sh
cadence app catalog upgrade-check <install-id> <absolute-source-path-or-git-url-or-builtin:id> \
  --expected-digest 'sha256:…' \
  --expected-generation '<catalog-generation>'
```

An upgrade keeps the installation ID, contexts, bindings, and
historical runs; it never creates a project. A verified operator in a writable
session can check and apply updates for any installed app at
`/apps/manage/<id>`: enter a source, select **Check update**, inspect the
proposed version, digest, changed files and compatibility/notes, then select
**Apply checked update**. The page pins the installed digest, catalog
generation, and checked new digest, and refuses stale proposals. Built-in
installs are prefilled with `builtin:<catalog_id>`, Git installs with their
recorded URL, and path installs start empty for an absolute path or Git URL.
The Social Content **Settings → App package** panel remains available.

`cadence app catalog upgrade-check` and `cadence app catalog upgrade` accept
absolute source paths, Git URLs, and `builtin:<catalog_id>`. In a hosted
AgenticOS container the remote CLI still has no app-management verbs, so the
Manage page is the board path for operators updating any app there.

## Test a Git-sourced update

Use a throwaway Cadence instance and a public HTTPS repository you control; do
not use a production workspace. The board's Git check accepts public HTTPS
repositories only, with the app bundle (including `app.md`) at the repository
root. Keep the app identity the same between versions and change the manifest
version and at least one bundle file so the new digest differs.

1. Push a valid v1 bundle to the repository's default branch, then install it
   from the Apps page using the Git-source check/install flow.
2. Push a v2 bundle to that same branch and repository.
3. Open the installed app's `/apps/manage/<install-id>` page. Its package source
   should be prefilled with the repository URL. Select **Check update**, review
   the v2 proposal and changed files, then select **Apply checked update**.
4. Confirm the installed version is v2. Repeating **Check update** without
   another bundle change should report that the package is up to date.

Use a public repository without embedded credentials. For a branch other than
the default branch, select the Git ref supported by the Git-source check when
installing and checking the update.

Installing or updating is the operator's consent for the exact installed
digest when the bundle passes local execution validation. `cadence app catalog
approve <install-id> --digest <digest>` explicitly approves the exact current
digest for supported local artifact steps; `revoke` with the same arguments
withdraws local capabilities for that digest and does not change legacy
project approvals. Either command refuses a stale digest. Approval of an app
bundle does not approve an individual run or authorize outward effects.

```sh
cadence app catalog upgrade <install-id> <absolute-source-path-or-git-url-or-builtin:id> \
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
the flag. The remote CLI (`cadence --org …`) has no app-management verbs. In a hosted
AgenticOS container, operators can update any installed app from its
`/apps/manage/<id>` page; the same CLI commands accept `builtin:<catalog_id>`
for embedded bundles.

This transport currently handles the package bundle and Cadence-owned app
metadata. Host-managed per-install SQLite storage and staged schema/data
migrations belong to the separate App storage contract; they must be integrated
before an app upgrade that changes its data schema is allowed.
