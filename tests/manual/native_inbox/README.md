# CAD-1015: owned native-session / inbox probes

These are **manual, opt-in real-runtime experiments**, not automatic CI tests and not evidence that CAD-1015's new delivery feature is implemented. They make real model calls using the operator's existing credentials. Run providers sequentially on the shared host.

## Native protocol probe

```sh
CAD1015_DOGFOOD=1 python3 tests/manual/native_inbox/native_probe.py pi
CAD1015_DOGFOOD=1 python3 tests/manual/native_inbox/native_probe.py codex
CAD1015_DOGFOOD=1 python3 tests/manual/native_inbox/native_probe.py claude
```

Each command starts one process it owns under a fresh short `/tmp/c1015-*` directory. The model runs one controlled hold script; the probe sends an amendment while that tool is blocked, then releases it. It asserts the amendment joined the original run and no completion was observed before release. Pi and Codex also receive a separate follow-up. Codex receives a forged/stale expectedTurnId and must reject it.

Pi additionally gets a deliberately idle `steer`: the report records whether the native queue carries it into the next run. **That is a negative safety experiment, not desired behavior.** Native steer accepting idle input means Cadence cannot safely implement a turn-bound nudge by forwarding this command alone. Claude stream-json likewise has no expectedTurnId guard. A host-side active-turn lock alone cannot prove that the provider has not already finished before its completion event reaches the host.

The report includes output tokens, event types and acceptance dispositions, not credentials/raw environment/tool output. Credential files are copied into the owned private temp root when needed and removed after process cleanup. Provider threads/processes are new; the script never connects to the shared Codex daemon or an existing Claude session. Only owned process groups may be signalled on cleanup.

## Existing Cadence inbox baseline

```sh
CAD1015_DOGFOOD=1 python3 tests/manual/native_inbox/cadence_probe.py
```

This starts an **independent temporary Cadence daemon**, a passive observer inbox and three sequential managed providers. It validates:

- busy-tool observations;
- current managed nudge refusal (recorded separately from normal queued work);
- kickoff and follow-up results with distinct turn ids and correct message correlation;
- non-consuming repeated inbox peek;
- explicit ack watermark and empty reader inbox afterwards;
- explicit agent stop and temporary daemon shutdown.

It reports the exact Cadence binary/version. By default it tests the installed binary; to test the lane build, pass an absolute binary path:

```sh
CAD1015_DOGFOOD=1 CAD1015_BINARY=/absolute/path/to/cadence \
  python3 tests/manual/native_inbox/cadence_probe.py
```

`CAD1015_PROVIDERS=claude` (or comma-separated pi,codex,claude) narrows a rerun. This is not an authorization or native-live-steering test. The daemon uses a temporary tracker/config/state root, not `~/pm` or production state. Pi's temporary model allowlist contains only `devin/swe-2-high`, pinned `pi-devin@0.2.1`; tool admission for the Claude hold command is explicit. No bypass-permissions option is used. Codex and Claude configuration/credential copies are isolated and removed after cleanup.

## Recorded initial evidence

Observed versions: Codex 0.160.0, Claude 2.1.287, Pi 1.0.0. Initial baseline used Cadence `81b2507ded21ab7268439591c66507455cc2cbeb`, **not** the lane's source head. All three native busy-turn probes observed amended output in the original run. All three Cadence baseline inbox probes observed correlated results and peek/ack behavior. Managed `--nudge` was refused for all three, as expected today.

The Pi idle-steer experiment returned `success:true`, disposition `queued`, then the next run output `FOLLOWUP_CAD1015 IDLE_CAD1015`. This is the regression condition the implementation must prevent for turn-bound messages.

Initial failed attempts were fixture/admission issues, retained in the ticket evidence: missing temporary Pi model allowlist; Claude `allowed_tools` passed as a string rather than a list; then an RPC registration used an object where Cadence expects serialized `params`. None justified bypassing a permission gate. The corrected Claude test used `params` containing serialized JSON with an explicit allowed-tools array.

## Remaining CAD-1015 work

Design runtime-side turn/session preconditions for Pi and Claude, write adversarial and completion-vs-send race tests first, then extend the existing registry/adapters and Cadence concurrent-control path. Do not replace the actor's serialized task queue or classify native acceptance/idle as task success. End-to-end validation of the **new** live-delivery feature and exact-head independent reviews are still pending.
