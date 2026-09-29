# Admitted focused test recipe for external reviewers (CAD-794)

External reviewers (for example Codex sessions) have no pane and no
managed endpoint, so they hold no build-slot identity of their own:
`cadence build-slot status` rejects them as underivable callers and
`cadence build-slot run` cannot bind them. Their supported path is
CAD-230's daemon-launched runner — `cadence build-slot launch <recipe>
--project cadence --worktree <lane>` — which runs a **fixed**
`build.recipes` entry from the `cadence` project's `project.yaml`.
Nothing about the command comes from the request: unknown recipes,
forged callers and any attempt to smuggle command, cwd or env text
are refused before anything spawns (see "Refusals" below).

## Status

This document is the reviewable proposal. The YAML in the next section
is **operator-owned project configuration**, applied by the operator to
the `cadence` project's `project.yaml` out of band (separately tracked
PM config — never edited from a lane). No daemon restart is needed: the
daemon re-reads `project.yaml` on every launch (`runner::resolve` lists
the PM dir per call). Ticket acceptance stays open until an external
operator session launches the applied recipe in a CAD lane and the
measured result is recorded (see "Acceptance measurement").

## Proposed `build.recipes` entry

Recipe name: `focused-confinement-kickoff`. The operator adds this
verbatim as a top-level `build:` block in the `cadence` project's
`project.yaml` — merging only the `focused-confinement-kickoff:`
mapping under an existing `build: recipes:` when one is already
present — filling in only `<HOST SUITE LOCK>` with the host's
designated suite-lock path:

```yaml
build:
  recipes:
    focused-confinement-kickoff:
      kind: test
      cwd: .
      env: [PATH, RUSTUP_HOME, CARGO_HOME]
      argv:
        - /bin/sh
        - -c
        - |
          set -eu
          : "${CADENCE_RUNNER_ID:?build runner id missing}"
          R=/tmp/cad-focused-${CADENCE_RUNNER_ID}
          umask 077
          mkdir -p "$R/home" "$R/tmp"
          export HOME="$R/home"
          export XDG_CONFIG_HOME="$R/home/.config"
          export XDG_DATA_HOME="$R/home/.local/share"
          export XDG_STATE_HOME="$R/home/.local/state"
          export TMPDIR="$R/tmp"
          export CARGO_BUILD_JOBS=4
          export CADENCE_SUITE_LOCK=<HOST SUITE LOCK>
          exec cargo test --locked --features test-seam \
            --test pi_worker_confinement --test issue_kickoff \
            -- --test-threads 2
```

### Why it is shaped this way

| Ticket requirement | How the entry meets it |
| --- | --- |
| Fixed argv/cwd/env, `kind: test` | `argv` is a literal list (`/bin/sh -c` + fixed script text); `cwd: .` pins the checkout root; `env` names only passthroughs; `kind: test` draws from the test slot pool. The launch RPC accepts only `recipe`, `project`, `worktree`, `wait_secs` — there is no field for a caller command or filter. |
| `CARGO_BUILD_JOBS=4`, `--test-threads 2` | Exported and passed as fixed text inside argv, matching the lane convention in `AGENTS.md` / `CONTRIBUTING.md`. Shared-host lanes use the same bounds. |
| Short isolated HOME/XDG/TMPDIR | Derived from the daemon-set `CADENCE_RUNNER_ID` (`/tmp/cad-focused-run-<32hex>`), unique per run and well under the 107-byte unix-socket limit. The script aborts if the id is ever unset rather than colliding on one directory. |
| Toolchain resolution under an isolated HOME | `RUSTUP_HOME`/`CARGO_HOME` pass through from the daemon's environment (the lane convention); `cargo` itself resolves via the allowlisted `PATH`. The operator confirms the daemon environment carries a cargo-bearing `PATH` (and the rustup homes) when applying the entry. |
| Trusted `CADENCE_SUITE_LOCK` | Recipe `env` rejects every `CADENCE_*` name, so the lock cannot ride the caller-influenced allowlist. It is baked into the fixed argv instead — a trusted value the operator sets once at apply time, never a request field. |
| No mutable lane wrapper | The whole invocation is inline in argv. It does not call a repo script that a dirty lane checkout could rewrite while the receipt still claims the fixed recipe ran. (The recipe intentionally tests lane *source*; only the harness invocation is immutable.) |
| No credentials in argv/logs | `env_clear` at spawn wipes everything except the three allowlisted names plus the daemon-set runner id/digest. No token, issuer or org name is allowlisted; the focused fixtures build their own temp tracker/state and never read the host `~/pm`. |

## External reviewer runbook

From a shell that proves operator identity. A pane-less external
reviewer has no pane agent and no managed endpoint, so
`launch_requester` admits it only as the proven operator — an
arbitrary daemon-reachable shell is refused, and the refusal names
the failed proof. In practice this means an attached operator shell:

```bash
cadence build-slot launch focused-confinement-kickoff \
  --project cadence --worktree <lane-worktree-path>
```

The command prints the runner id, streams the recipe log, and exits
with the recipe's exit code. Variants:

- `--detach` prints the runner id and returns at once; read the result
  later with `cadence build-slot runner <id>`.
- `--worktree` must be a checkout of a repo registered to project
  `cadence` (a lane worktree qualifies). Always pass it alongside an
  explicit `--project cadence`: in that case the CLI does not forward
  the cwd, and the runner would otherwise default to the project's
  first registered checkout instead of the intended lane.
- `--wait-secs <n>` bounds only the slot queue (default 600); a queue
  timeout never starts the recipe.

The receipt (`build-slot runner <id>`) records the recipe name, the
launch digest, the source `HEAD` the intent bound, `dirty` when the
checkout had uncommitted changes, the exit code/signal, and the log
path under the state dir. The log holds the full `cargo test` output.

## Refusals (no runner starts)

- Unknown recipe: `Unknown recipe '<name>' for project 'cadence' — it
  defines: focused-confinement-kickoff. Recipes come only from
  build.recipes in the project's project.yaml`.
- Command/cwd/env overrides: no request field names a command — only
  `recipe`, `project`, `worktree` and `wait_secs` are accepted (`--worktree`
  itself is legitimate), so any command-shaped extra field is refused:
  `build-slot launch takes only recipe, project, worktree and wait_secs
  — '<field>' is refused`.
- Foreign checkout: `'<path>' is not a checkout of a repo registered to
  project 'cadence'`.
- Underivable, unenrolled caller: `build-slot launch needs a pane
  agent, an enrolled managed endpoint or the proven operator`.
- Source moved while queued: the gate never opens; the receipt names
  the old and new `HEAD` and the reviewer launches again.

## Acceptance measurement

For the first external launch after the operator applies the entry,
record on CAD-794:

1. Request-to-result wall time (launch command start to exit receipt),
   the runner id, source `HEAD`, and pass/fail per test target.
2. Whether the next relevant review iteration used this recipe instead
   of waiting for a full CI round trip. Do not claim CI time savings
   unless the comparison is measured.

## Operator apply step (the remaining step after this PR)

1. Add the YAML above as a top-level `build:` block in the `cadence`
   project's `project.yaml`, replacing `<HOST SUITE LOCK>` with the
   host's designated suite-lock path. If a top-level `build:` mapping
   already exists, merge only the `focused-confinement-kickoff:`
   mapping under the existing `build: recipes:` — never nest a second
   `build:` inside `recipes:`.
2. From an external operator session, run the runbook command in a CAD
   lane and confirm the exit receipt carries the lane's `HEAD` and the
   focused test result.
3. Post the measurement from the previous section to CAD-794. No daemon
   restart, no lane change, and no `--admin` merge are involved.
