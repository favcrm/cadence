# CLI: which verb ships a build

Two loops, decided by the store's mode, not by what the caller types.

## Production: `cadence update`

One verb ships a build to a production store: it claims the rollout lease,
backs up the store, records the backup as the lease receipt, installs the
attested build side by side, drains, switches, health-checks and releases
the lease.

```
cadence update --check                                   # what would happen
cadence update                                           # newest approved candidate
cadence update --to <full-40-hex-sha>                    # pin a commit
cadence update --check --to <sha>                        # report (and verify) that sha
cadence update --rollback
cadence update --as operator:<name>                      # explicit audit label (optional)
```

These examples use the local org. The existing default-remote-org routing
limitation (CAD-1355) is unchanged: if your default org is remote, explicitly
select local with `CADENCE_ORG=local cadence update`.

`cadence update` is operator-only and proves it the same way
`rollout claim --as` does: the process must run outside every pane and
managed endpoint, which an agent, a pane descendant or an unproven
caller can never satisfy — a forged `--as` or scrubbed environment
changes nothing. `--as operator:<name>` is now optional: it only names
the audit label. Without it, the run is labelled from the proven local
OS identity (the real uid and pid), never from `$USER`,
`$CADENCE_ALIAS` or a client-supplied field.

On a runtime the tracker's `pm.yaml` marks image-managed (`[hosted]`
`lease` configured), `cadence update` refuses before touching anything:
AgenticOS owns the binary lifecycle there and ships updates by
replacing the container image. A `CADENCE_AGENTICOS_URL` integration or
a container marker alone is not a managed runtime — a native host that
talks to AgenticOS still self-updates. Updating an image-managed
deployment means shipping a new image, not running `cadence update`
inside it.

`--to` runs the same checks as `upgrade --sha`: the sha is on main, CI's
`test` job passed on that exact sha, the sha256 and manifest match, and the
build-provenance attestation verifies for the repository. A sha that is not
on main, or has no attested build, is refused with a sentence naming the
reason. Without `--to`, behavior is unchanged.

## Development: `cadence dev`

`cadence dev` (old name: `cadence sandbox`) is a disposable second Cadence
with its own state dir, tracker and board port (3110-3199).

```
cadence dev up [--name <n>] [--build <path>]   # create/start; writes the dev marker into the store
eval "$(cadence dev env)"                      # point this shell at it
cadence dev reload [--build <path>]            # restart from a local binary
cadence dev status | down | env | ls | reset <name>
```

`dev reload` defaults to the newest of the repo's `target/release/cadence` and
`target/debug/cadence`. It claims no rollout lease, checks no attestation and
takes no backup.

### What makes a store a dev store

A store may change build without the rollout lease, and may run an unattested
binary, only when both hold:

1. it carries the dev marker `<state>/.cadence-dev`, written by `dev up`, which
   names the store's own resolved path (a copy elsewhere proves nothing); and
2. its root is a direct child of the sandbox base (`$CADENCE_SANDBOX_ROOT`,
   else `$XDG_STATE_HOME/cadence-sandbox`).

`CADENCE_PROFILE=sandbox:<name>` is not consulted for this: exporting it on
any other store unlocks nothing. `dev reload` refuses (and stops and starts
nothing) on a plain `--state-dir`, a marker copied outside the base, the
production state dir, or an unmarked store with the profile exported.
Production's rules (lease, backup receipt, attestation) are unchanged.
A sandbox created before the dev marker existed (before #831) loses the
lease exemption until `cadence dev up` re-marks it: run `cadence dev up` once
per old sandbox after the update that ships this change.

## Recovery / internals: `rollout` and `upgrade`

These are hidden from `cadence --help`, work unchanged, and exist for recovery
and scripts. Prefer `cadence update`; it does their steps in order.

- `cadence rollout claim | status | release | handoff | backup | grant ...`:
  the durable lease that gates a build change and a schema crossing.
  `rollout status` with no backup receipt prints a hint naming `cadence update`.
- `cadence upgrade --sha <sha> | --latest-main [--dry-run] [--restart]`: the
  low-level verified install that `update` calls. `--allow-unattested` rolls
  back to a hand-built release already on disk.
