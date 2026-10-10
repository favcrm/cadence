# Rollout checklist by artifact

Use the block for the artifact being changed. For a new artifact type, copy one
block and fill in its owner, blast radius, order, rollback trigger, rollback and
verification before rollout. Record exact reviewed SHAs/digests and evidence in
the change's ticket; follow the repository's [delivery workflow](../../AGENTS.md#delivery-workflow)
for review, approvals and queueing.

## Shared rollout rules

Ship the smallest artifact that carries the fix. Expand before contract: keep
old and new interfaces working during rollout, then remove the old form only
after consumers have moved. Batch layers that share a restart. For tenant-scoped artifacts, roll out to a
test tenant first and expand in stages only after read-only verification; never
publish to a live account to prove a release.

## AgenticOS Worker (API)

- **Runner:** authorized operator; promotion PR, then the deploy workflow.
- **Blast radius:** all tenants; stateless and immediate.
- **Order:** review and merge the promotion, then dispatch `production-deploy.yml`
  pinned to the promotion merge's second parent (`source_sha`). Use the smallest
  artifact that carries the fix; do not wait for an unrelated image cycle.
- **Rollback trigger / decision:** failed read-only smoke or a production
  regression; operator decides whether to roll back.
- **Rollback:** redeploy the previous `source_sha`.
- **Verify:** run the read-only route smoke first. Any optional canary is only
  on the test account, never a live account; do not publish to prove a release.

## Runtime image (pins a Cadence build)

- **Runner:** authorized operator; repin PR, promotion and deploy.
- **Blast radius:** every tenant that takes the new image on cold start; awake
  tenants require a planned restart.
- **Order:** review the repin, promote and deploy; review pending migrations and
  take the required backup before applying them. Restart only the planned test
  tenant first, then expand in stages. Batch layers that share this restart.
- **Rollback trigger / decision:** failed tenant probe, failed smoke, or a
  migration/deploy failure; operator decides, and pauses expansion on failure.
- **Rollback:** redeploy the coupled previous image and Worker versions; restore
  migrated store data from the recorded backup when needed.
- **Verify:** require a real tenant probe and read-only smoke before expanding
  to the next tenant; a running/listed state is not proof that it answers.

## Cadence host binary

- **Runner:** operator, using the supported `cadence update` flow.
- **Blast radius:** one production host (board and daemon).
- **Order:** verify the approved candidate and its checks, then follow
  [the production update procedure](../../AGENTS.md#production-safety). Batch
  host changes that need the same image cycle with that cycle, not a separate
  restart.
- **Rollback trigger / decision:** update refusal, failed read-only health
  probe, or regression; operator decides whether to restore the previous
  release.
- **Rollback:** restore the previous release symlink through the supported
  operator recovery procedure; do not improvise a state rollback.
- **Verify:** read-only status/health probe on the updated host before any
  canary; never publish to a live account to prove a release.

## App bundle

- **Runner:** operator, using the bundle kit's `run.sh`.
- **Blast radius:** one installed app bundle in the selected host/tenant.
- **Order:** verify the reviewed bundle and digest, install to the test tenant
  first, probe it, then expand to further tenants in stages. Ship only the
  smallest artifact that carries the fix.
- **Rollback trigger / decision:** install refusal, failed real probe, or
  regression; operator decides whether to roll back.
- **Rollback:** use the kit's `run.sh rollback` to restore its previous bundle.
- **Verify:** read-only smoke and a real tenant response before expansion; any
  canary action is test-account-only and is never a live publish.

## Shared operator-script preflight

Pinned operator scripts should source `scripts/lib/operator-preflight.sh` and
run every applicable check before their first mutation. It defines functions
only; sourcing it performs no checks or other side effects. Supply the
Cloudflare CLI's verified read-only authentication command to
`preflight_cf_auth`; this repository has no existing `cf` invocation to copy.
Keep expected pins in reviewed script inputs, not inferred from a mutable latest
label. Pass a real, read-only tenant probe command; a state label is not a
probe.

```bash
source "$REPO/scripts/lib/operator-preflight.sh"
preflight_gh_auth || exit 1
preflight_cf_auth "${CF_READ_AUTH[@]}" || exit 1
preflight_sha "$EXPECTED_SHA" "$LIVE_SHA" || exit 1
preflight_digest_set "$EXPECTED_DIGEST" || exit 1
# For scripts that must NOT touch production:
preflight_state_dir "$STATE_DIR" || exit 1
preflight_probe "$CADENCE_BIN" --state-dir "$STATE_DIR" status || exit 1
# Production-targeting scripts omit preflight_state_dir and pin production explicitly.
# Only after every applicable preflight succeeds may the script mutate state.
```

Each refusal prints one plain reason without exposing credentials. Set the
expected SHA to the reviewed full 40-hex commit and the expected digest to the
reviewed artifact digest (not whitespace-only). `preflight_state_dir` is only
for scripts that must not touch production, such as sandbox, staging or
rehearsal scripts. Production-targeting scripts must not call it; they pin the
production state explicitly.

## Post-incident note

For each production miss, add one short paragraph as a comment on its ticket:
what happened, its cause, and the specific rule now preventing recurrence.
Link that ticket here. Keep incident records factual; do not turn an unverified
hypothesis into a rule.
