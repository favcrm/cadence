# Native live-session messaging (CAD-1015)

Cadence owns durable messages, caller policy, session ownership, task results and
recovery. Native adapters translate delivery; they are not a second broker.

## Delivery matrix

Observed runtimes: Codex 0.160.0, Pi 1.0.0, Claude Code 2.1.287.

| Intent | Managed Codex | Managed Pi | Managed Claude | PTY |
| --- | --- | --- | --- | --- |
| Follow-up `send` | Cadence next-turn queue | Cadence next-turn queue | Cadence next-turn queue | Existing queue/ready gate |
| Current-turn `send --nudge` | `turn/steer` with `expectedTurnId` | Owned runtime turn guard and custom steering input | Explicitly unsupported | Existing verified turn-bound paste |
| Interrupt | Existing `turn/interrupt` | Existing `abort` | Existing interrupt control | Existing provider mapping |
| Resume | Existing owned-thread reopen | Existing owned session file | Existing owned session reopen | Existing ownership/reattach rules |
| Observe/result | Existing events, wait and inbox | Existing events, wait and inbox | Existing events, wait and inbox | Existing explicit report/notice/pull |

The registry's `capabilities.native_turn_steering` describes potential support.
`agent show`/`agent list` additionally expose `native_turn_steering_enabled` for
the live adapter. Pi requires command discovery from the generated guard's
exact resource path, not merely a matching command name. A missing guard
refuses native input; it does not use Pi's unguarded `steer` command instead.
The matrix documents tested versions, not certification of every future runtime.
Experimental Codex queues and `thread/inject_items` are not used.

## Operator/PM usage

```sh
cadence send worker --text 'After this task, investigate the regression.'
cadence send worker --nudge --message clarification-1 \
  --text 'Reuse the existing email provider; retain authentication and tests.'
cadence agent wait worker --until reported --timeout 10m
cadence inbox observer --peek --reader pm
cadence inbox ack observer <seq> --reader pm
```

Nudges retain the existing operator-or-recipient-PM authority gate. A peer's
warning can use an ordinary informational message; being a peer does not grant
steering, operator, approval or task-mutation authority. Nudges are limited to
500 Unicode scalars and take no task, reply-to, priority or supersession option.
Sender, session, generation and turn references cannot be supplied by the caller.
Cadence binds the active report-owing message at admission, then the runtime
checks the exact target before queuing input. Idle or closing targets are
skipped, never retained for the next run.

Native admission goes directly through the existing adapter-control seam while
the actor waits for its original result. There remains one stdout reader. The
turnless row is atomically inserted and claimed as `submitting`; the regular
actor queue cannot claim it. A duplicate durable envelope returns its original
disposition, without another provider call. Reusing an id with changed content
is a conflict. This is broker idempotency, not a claim of provider idempotency.

## Disposition is not task completion

A native receipt can have message state `completed` with:

```json
{"status":"accepted","outcome":"queued","application":"unconfirmed",
 "via":"native_turn_steering","target_message":"original-kickoff-id"}
```

That means the delivery attempt has settled, not that the model applied the
input or completed the kickoff. It does not idle the agent, finish the parent,
route a task result, change criteria or grant approval. Active turn tokens remain
subject to the existing response redaction. Use `target_message` for public
correlation; do not expose tokens through differently named fields.

`skipped_inactive` is cancelled/non-applied; a definite refusal is failed; a
missing or ambiguous receipt is non-fencing `unknown`. Unknown nudges are never
automatically replayed. Stop/restart can win over an in-flight attempt; a late
provider answer is recorded without overwriting that terminal evidence.
Events are `native_steer_submitting`, `native_steer_disposition`, and the existing
`nudge_cancelled`. The disposition event's `recorded` flag describes a database
update, never model application. Original kickoff completion still comes from
its own matching provider result.

## Runtime guard and limitations

Pi's generated guard loads before other extensions. It binds the daemon-minted
run token before kickoff, opens admission on `turn_start`, closes it on
`turn_end`/`agent_end`, and resets on settlement or session change. The predicate
and custom-message enqueue have no intervening await. It rejects idle, closing,
aborted and foreign-token input. Commands append correlated versioned receipts;
a generic successful slash-command response is not acceptance proof. An exact
unused binding can be abandoned after definite prompt refusal, never an active
run. Each run admits at most 64 distinct guidance ids. A context hook excludes
old-run guidance from later model requests. After settlement the adapter confirms
`clear_queue` before allowing another kickoff; uncertain cleanup fences the
kickoff instead of risking replay. Both filtering and cleanup matter when input
was accepted just before cancellation.

Codex validates `expectedTurnId` inside the app-server. Wrong or missing turn ids
in a successful reply are unknown, not accepted. The adapter releases
reader-shared locks before waiting, so a quota notification cannot block receipt
processing. There is no steer-to-start/follow-up fallback.

Claude's tested stream input can amend busy work, but lacks an atomic
expected-turn predicate. Its cross-session socket queues messages for later
submission; idle notifications are not conditional send. A `UserPromptSubmit`
hook is not a substitute: timeout/non-blocking hook errors fail open. Strict
native nudges therefore refuse before delivery. Ordinary queued work and inbox
observation remain supported.

This slice adds bounded guidance, not a new typed objective-replacement API.
CAD-160 still composes the initial/follow-up `send --task` objective and outstanding
criteria. Native guidance stays in that same conversation and never changes the
stored objective/criteria; it cannot attach another task. Guaranteeing a model's
semantic interpretation of a clarification, or explicitly replacing an objective,
remains a typed-amendment policy gap, not something inferred from an ack.

## Validation

See `tests/manual/native_inbox/README.md` and `EVIDENCE.md`. Opt-in probes use
newly owned sessions and temporary state, never production sessions. Direct
runtime probes are not Cadence end-to-end evidence. The lane-native smoke mode
is `CAD1015_NATIVE_STEERING=1`; use an absolute lane binary and preserve build,
revision/artifact, runtime versions, failure cases and cleanup evidence.
