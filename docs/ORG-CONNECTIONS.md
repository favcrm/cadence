# Organization and connection selection (CAD-657)

This first increment adds a client registry and local destination selection.
Remote records can be registered and inspected but **cannot execute commands**:
CAD-539's authenticated transport is not implemented. Selecting a remote record
returns an error before any local socket, tracker or daemon operation. Registration
is configuration, not proof of membership or successful login.

```sh
cadence org add local --connection dev \
  --local-state-dir /absolute/dev/state --tracker-dir /absolute/dev/tracker
cadence org add acme --connection cloud \
  --endpoint https://cadence.example.com --org-id company-acme
cadence org list
cadence org inspect local --connection dev
cadence org switch local --connection dev
cadence --org local --connection dev agent list --json
```

`org` is a user-chosen organization label; a standalone local installation does
not have or assert a cloud organization ID. Remote connections record the issued
organization ID separately. Local connections bind both state and tracker roots;
registration does not provision either. An organization with several connections
requires an explicit connection unless that organization's selected default exists.

The registry is `$XDG_CONFIG_HOME/cadence/orgs.json`, or
`~/.config/cadence/orgs.json`, separate from runtime state. It contains no tokens.
List/inspect report local peer authority or `not_connected`; they do not claim
verified remote credentials. Writes require the existing CLI operator proof, use
an exclusive interprocess lock and atomically replace the file after fsync.
Registry and lock files reject symlinks/nonregular files; registry reads/writes
are bounded to 64 KiB. Configuration ancestors are assumed to be user-controlled: this
client configuration is not an authorization boundary against arbitrary same-UID
filesystem writers. Remote membership remains server-authoritative.

Selection precedence:

1. Existing `--state-dir`, `CADENCE_STATE_DIR`, `CADENCE_PM_DIR`, `CADENCE_HOME`
   and `CADENCE_PROFILE` bindings preserve existing behavior and take precedence
   over the saved default. Explicit `--org`/`--connection` with these bindings is
   rejected, rather than mixing roots. Use a clean operator shell to override.
2. `--org`/`--connection` select a connection for this invocation only.
3. The saved default selects otherwise. No saved default preserves legacy behavior.

Each command reads one locked snapshot, then binds state and tracker in its own
process. Later switches cannot redirect that command. Existing managed workers
already receive `CADENCE_STATE_DIR` (PTY, Claude and Pi adapters); this binding
continues to win when commands reconnect. Legacy managed callers without that variable retain explicit `--state-dir`
or existing HOME/XDG state resolution, with their existing tracker binding. Any
managed alias bypasses the org registry entirely and rejects explicit org/connection
overrides. Alias presence does not authenticate an actor; daemon proofs still apply. Switching never edits agent,
team, message, turn or review records. Selected local commands print the label,
connection and runtime path to stderr; structured stdout remains compatible.

Not delivered in this increment: browser/token integration, authenticated remote
RPC, credential/audience checks, board indicators, remove with active-work checks,
and transferring any work. Remote identity and worker enrollment remain CAD-539
work. No automatic credential fallback or migration is provided.

Validation: `cargo test --test org_connections -- --test-threads 2` covers actual
Unix RPC selection, separate tracker initialization, in-flight switching,
reconnection with inherited bindings, remote/unreachable failure without fallback,
concurrent registry writes, conflicting selection and operator/agent/detached
proof. Compilation and tests run in CI because local build-slot admission is not
available; formatting is checked locally. Authorization counterfactual proof
(test fails with operator guard removed) remains a required independent review
step before this increment is eligible to merge.
