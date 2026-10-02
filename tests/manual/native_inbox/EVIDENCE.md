# CAD-1015 — initial dogfood evidence

## Scope and versions

Manual owned-session probes; no production daemon restart, shared-server connection or takeover. Native CLI versions observed: Codex 0.160.0; Claude 2.1.287; Pi 1.0.0. Pi used devin/swe-2-high with the pinned pi-devin 0.2.1 extension. Codex used gpt-6.1-sol; Claude used its native default. Tests were sequential.

The end-to-end baseline used installed Cadence `0.1.0-beta.2+81b2507ded21ab7268439591c66507455cc2cbeb`. This is **not** evidence of branch-build behavior or the new feature being implemented.

## Final native probes

Commands: `CAD1015_DOGFOOD=1 python3 tests/manual/native_inbox/native_probe.py <provider>`.

| Provider | Busy controlled tool | Amendment in original run | Completion observed before release | Separate follow-up | Additional result | Exit |
| --- | --- | --- | --- | --- | --- | --- |
| Pi | observed | BASELINE AMENDED_CAD1015 | no | observed | idle steer queued and leaked into next run | 0 |
| Codex | observed | BASELINE AMENDED_CAD1015 | no | observed | stale expectedTurnId rejected | 0 |
| Claude | observed | BASELINE AMENDED_CAD1015 | no | not exercised by this native probe | stream-json amendment joined original single-result run | 0 |

Native process cleanup returned exit 0 for each. Syntax parsing and missing-opt-in refusal checks passed for both probe scripts.

**Safety discovery:** Pi idle `steer` returned `success: true`, `data.disposition: queued`; next prompt produced `FOLLOWUP_CAD1015 IDLE_CAD1015`. This reproduces the stale-input risk, not desired Cadence behavior. Neither a successful native response nor a host active-turn snapshot establishes exact-turn delivery for Pi. Claude stream-json has no expected-turn precondition in the tested input frame. Before adding a strict turn-bound managed nudge, require a runtime-side generation/run predicate or explicitly refuse unsupported semantics.

## Final Cadence inbox baseline

Command: `CAD1015_DOGFOOD=1 python3 tests/manual/native_inbox/cadence_probe.py`.

One new temporary daemon at `/tmp/c1015-e2e-h9h88zgw/state`, private temporary PM configuration, fresh passive observer inbox, new providers executed sequentially. The daemon exited 0. Final script exit 0.

| Provider | Original result | Queued follow-up result | Distinct turn ids | Repeated peek non-consuming | Ack cleared reader inbox | Managed nudge |
| --- | --- | --- | --- | --- | --- | --- |
| Pi | BASELINE / completed | BASELINE AMENDED_CAD1015 / completed | yes | yes | yes, seq 3/4 | explicitly refused |
| Codex | BASELINE / completed | BASELINE AMENDED_CAD1015 / completed | yes | yes | yes, seq 7/8 | explicitly refused |
| Claude | BASELINE / completed | BASELINE AMENDED_CAD1015 / completed | yes | yes | yes, seq 11/12 | explicitly refused |

A normal message sent while busy was processed as a **separate next turn**, not an active amendment. Result payloads were matched by message id and native/Cadence turn id. Existing inbox delivery works; native live delivery is the missing integration.

## Earlier attempts / corrections

Retained rather than calling every run green:
1. Temporary Pi config omitted `models.allow`; the launch correctly refused. Fixed the isolated test config, not operator production policy.
2. Claude CLI `--param allowed_tools=[...]` stored a string, while the adapter consumes an array. The hold command was correctly denied; no bypass was added.
3. Direct registration initially supplied an object for `params`, but Cadence's registration protocol expects serialized JSON. Corrected to a serialized object containing `permission_mode: dontAsk` and the narrow `allowed_tools` array. The third Claude baseline run passed; final unified run passed all three.

## Not yet validated / not implemented

No source runtime adapter/daemon change has been made in this milestone. New live-delivery end-to-end behavior, sender authorization/adversarial tests, runtime-side stale-turn guards, shutdown/reconnect/unknown-delivery safety, objective/acceptance-state integration and exact-head independent reviews remain pending. Current task is doing, not complete; this evidence validates the protocol seam and baseline only.

## Evidence retention

The ticket retains the JSON native and Cadence inbox reports. Reports contain controlled probe text, event types, short-lived test ids and dispositions; credentials/configuration and raw tool I/O are not attached. Credential copies in final owned test roots were removed after the corresponding processes stopped. The implementation's guard design continues separately.
