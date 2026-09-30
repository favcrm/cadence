# Pi model selection and refresh

This is the shared runbook for [repository instructions](../AGENTS.md) and
[managed Pi policy](BOARD.md#pi-in-pmyaml--the-operators-pi-policy-cad-559).
Model preferences are changeable operating choices, not an exclusive-vendor
rule. Runtime permissions, credential custody, confinement and delivery gates
are not preferences and remain enforced.

## Selection and authority

1. Honor the authenticated operator's explicit provider/model and effort for
   the task, subject to applicable higher-priority instructions and runtime
   permissions. Do not replace the requested provider with the same vendor's
   model through another gateway. High effort is not max effort.
2. When no model is requested, use the operator-controlled configured default.
   Documented preferences do not override an explicit selection or that
   configuration. Pass the selected model and effort explicitly for launches
   whose choice matters, especially reviewers.
3. Use an exact `provider/id` from the installed Pi catalog, not a display name,
   wildcard or guessed alias. For example, the catalog observed on 2026-09-30
   lists **`openai-codex/gpt-6.1-sol`**. The requested spelling
   `openai-codex/gpt-6-1-sol` is not that exact ID: show the discovered spelling
   to the operator rather than silently normalizing it.
4. `pm.yaml` `[pi].models` is the runtime authority. Check the applicable
   `worker_allow` or `master_allow` when present; otherwise `allow` applies.
   A present empty role list permits nothing. Absence of `[pi]` or an allowed
   model is a blocker, not permission to fall back. The operator owns changes
   to these lists and defaults; agents do not edit production pins or broaden
   them to authorize themselves. A launch request alone does not add a pin.
5. Never route workers or reviewers through `openrouter/*`. The existing
   operator-pinned production master exception is unchanged. Neither a new
   catalog entry nor a preference update creates another exception.

Two independent reviewers, their native identities and exact-head evidence
remain required by [AGENTS.md](../AGENTS.md). Prefer a different model vendor
from the author when the approved selection permits; a different alias or
model alone does not prove independence. Model selection is not merge approval.

## Discover and verify

Before dispatch, and after a relevant change, record the requested selection,
exact resolved ID, effort, catalog/runtime versions, authorization source and
sanitized readiness result on the ticket or handoff. Discovery is not proof of
working authentication, available quota or permission to launch.

For a built-in provider, the operator can inspect without launching a worker:

```bash
pi --version
pi --no-extensions --list-models openai-codex
pi auth check --provider openai-codex --no-refresh --json
readlink ~/.local/bin/cadence
```

Do not use `--credentials`, credential-print commands, or copy `auth.json`
into notes, prompts or the repository. An expired credential with
`--no-refresh` needs operator renewal, not an unattended credential change.
The local built-in catalog observation above did not verify a managed launch.

Built-in `openai-codex` models do not need `pi-devin`. Devin models need the
operator-pinned `pi-devin@<version>` package from `[pi].providers`; discovery
must load that approved extension explicitly with `-e`. Check live pricing
and access with the provider's catalog/billing information (`devin models list`
for Devin), not a permanent price table here. Free pricing does not imply
unused subscription quota.

For an authorized managed launch, use the exact discovered ID and effort:

```bash
cadence join <pm> pi --alias <reviewer> --role reviewer --detach \
  --model openai-codex/gpt-6.1-sol --effort high
```

This example is conditional on the applicable allowlist, current instructions,
authentication and task authorization; it is not a production rollout command.
Cadence checks the actual provider/model and thinking level through Pi's
`get_state`. Require a successful managed open and inspect
`cadence agent show <reviewer>` for `model_reported`, `model_effective`,
`effort_reported` and `effort_effective` before accepting review output;
configured/desired fields alone are not proof of the running selection.
Unsupported or clamped effort, a different resolved model,
unavailable authentication/quota, or a missing pin stays blocked. Do not turn
off confinement or other safeguards to make a provider work. Confined Pi
workers cannot use Devin's CLI-backed credential path; consult the managed
policy rather than assuming a preference overrides that restriction.

## Switch or recover an owned reviewer

A quota/authentication/model error is a blocker to record, not an automatic
switch or an invitation to try every model. Do not repeatedly retry a
persistent quota refusal. CAD-601 owns diagnosis of managed Pi versus manual
Devin quota discrepancies; a quota-shaped error alone does not prove their
underlying cause or that a manual test and managed launch are equivalent.

The operator or the reviewer's own PM may change an already-authorized
selection. Inspect the agent, partial output and message states first. Resolve
any running or unknown turn through the existing recovery procedure; do not
interrupt unrelated work or assume a failed acknowledgement means no work ran.
Only then, for an owned idle/stopped agent:

```bash
cadence agent set <reviewer> --next-launch \
  model=openai-codex/gpt-6.1-sol effort=high
cadence agent stop <reviewer>
cadence agent resume <reviewer> --detach
cadence agent show <reviewer>
```

`--next-launch` changes saved launch parameters, not the live process. Stop and
resume preserve message/history records. Inspect queued instructions and
resubmit only work established as unperformed; do not blindly duplicate a
kickoff or mutation. A fresh reviewer must review the named head itself; no
verdict is fabricated or borrowed from the failed reviewer. A changed code
head still voids old receipts. If the approved replacement is unavailable,
report the blocker rather than choosing an unapproved fallback.

## Keep choices current

Revalidate at each task boundary and when the operator changes the requested
model/effort, a provider refuses auth/quota, an ID disappears, catalog metadata
changes, or Pi/Cadence/provider packages change. There is no automatic updater
or automatic fallback implemented by this document.

- **Already permitted model:** record the operator's new choice and verification,
  use explicit launch/next-launch parameters, and leave unrelated agents alone.
  No exclusive-provider prose needs rewriting for each model generation.
- **New model/provider or changed permission:** the operator reviews discovery,
  capabilities, pricing, credential access and confinement compatibility, then
  updates the exact applicable policy entries and defaults under the existing
  controlled configuration procedure. Preserve other entries. New extension
  providers require reviewed version/content pins; built-in model selection
  does not require an extension install.
- **Stale catalog/runtime:** the operator may authorize `pi update --models` or
  the appropriate reviewed runtime/package rollout. Managed Pi uses private
  offline configuration, so a shell catalog refresh is not evidence that its
  catalog changed. Reverify the managed effective ID/effort and record versions;
  never restart production or install shared packages as an incidental fix.
- **Instruction/preference change:** use a ticket and reviewed version-controlled
  update to this runbook; update AGENTS.md only when the durable selection rules
  change. Keep preferences here instead of duplicating provider/price tables.
  Changes to rules, risk policy or gates retain the existing independent-review
  and recorded operator-approval requirements, not self-approval.
- **Adoption and rollback:** record the committed instruction revision and
  operator-authorized runtime selection. Notify agents at the next task boundary
  and refresh their briefing/context through the normal owned-session procedure.
  Ensure applicable harness instructions reflect the approved revision; a
  running context is not assumed to reload edited files. An unreviewed draft
  cannot authorize its own review or override higher-priority instructions.
  On failure, keep the reviewer stopped or restore the previous still-permitted
  exact model/effort through next-launch parameters with authorization and
  reverify. Revert policy/instruction revisions through their normal review and
  rollout owners; do not silently replay messages or roll back other agents.
