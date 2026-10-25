# Native live-session messaging (CAD-1015)

Cadence owns durable messages, caller policy, session ownership, task results and
recovery. Native adapters translate delivery; they are not a second broker.

## Delivery matrix

Observed runtimes: Codex 0.160.0 and 0.161.0, Pi 1.0.0, Claude Code 2.1.287.

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

## Local connect recipe (Codex, Pi, Cursor, omp)

CAD-1814's first wave is Codex, Pi, native Cursor (`agent`) and omp. Claude
is deferred while its usage is unavailable; cloud/hosted runtimes and custom
skills/MCP/memory are later work. On 2026-10-25, codex-cli 0.163.0 and Pi
1.4.0 produced real replies and routed peer results in disposable state.
Cursor is blocked on native login; omp 18.12.6 launches but its private
native store has no `openai-codex` login. Neither is a model-reply PASS.
The owned local runtime-to-daemon route below is separate from
`agent connect`, which binds a local actor to a hosted workspace. Runtime
versions are evidence, not certification of native steering.

1. Instance and state, on a non-production board port:
   `cadence dev up --name <n> --build <cadence>` then
   `eval "$(cadence dev env <n>)"`. Pin the destination explicitly on every
   later command (`--state-dir <root>/state`) and confirm
   `cadence dev status <n>` reports that same `root` before reading or
   mutating; `settings agents show` reports the settings revision separately
   (step 3). This keeps a previously sourced
   `CADENCE_STATE_DIR`/`CADENCE_PROFILE` from retargeting the instance. No
   manual or exported credential copies are needed for the exercised
   Codex/Pi route: Codex reads its normal configuration, while a managed Pi
   worker gets the adapter's private auth.json copy under the state dir
   (`PI_CODING_AGENT_DIR`; never env/argv, never `~/.pi` inheritance).
2. Provider readiness is separate from Cadence:
   `codex login status`, `pi auth check --provider <provider>`. For native
   Cursor, `agent status` alone is insufficient: this host reported a
   successful cached login while both `agent models` and real inference
   refused authentication. The operator must complete `agent login`.
   OMP uses its native `agent.db`, not Pi's auth.json. Do not copy an entire
   operator database or export credentials to the command line. After its
   private worker dir exists, the operator can authenticate that store with
   `PI_CODING_AGENT_DIR=<sd>/agents/<omp>/pi omp login openai-codex`.
   Missing native auth or quota is a blocker, not a delivery failure to
   substitute another account/provider around.
3. Launch (no profile, skill, messaging MCP or memory service required):
   `cadence codex --alias <codex> --role pm --cwd <repo> --detach`,
   `cadence cursor --alias <cursor> --role pm --cwd <repo> --model <native-model> --detach`,
   and `cadence join <group> pi --alias <pi>
   --model <provider/model> --no-bootstrap --detach`. OMP uses
   `cadence join <group> omp --alias <omp> --model <provider/model>
   --no-bootstrap --detach`. There is no standalone `cadence pi` verb.
   Pi/OMP workers require an operator-authorized worker model in
   `Settings > Agents`; no silent provider fallback is used. Cursor uses
   the native `agent` harness, not Cursor through Pi: the unsafe Pi
   transport remains refused. A startup refusal lands in `attention`.
   Seed only the intended row against the pinned destination as the operator;
   re-read the revision before each mutation:

   ```sh
   cadence --state-dir <sd> settings agents show    # take .revision
   cadence --state-dir <sd> settings agents provider put pi \
     --kind pi-extension --expected-revision <rev1>
   cadence --state-dir <sd> settings agents show    # new .revision
   cadence --state-dir <sd> settings agents model put <provider/model> \
     --provider pi --scope worker --expected-revision <rev2>
   ```

   `--expected-revision` is required on both mutations; a stale token is
   refused with a conflict, so take the fresh one instead of retrying the
   same call.
4. Send and correlate: `cadence send <alias> --message <id> --text '…'
   --wait`. The terminal receipt carries the provider's own `state`,
   `result.text` and `result.turn_id`; `agent show` reports the session id and
   `model_effective`/`model_reported`. `cadence message send` without
   `--wait` answers once the envelope is durable (`queued`).
5. Reply routing: `--reply-to <peer>` routes the finished turn's result to
   that peer as a `worker_result` message with the recorded sender and
   recipient uids. The routed copy has no `reply_to`, so it cannot loop; one
   send produces the reply hop without a second manual delivery.
6. Operator surface: the Agent page's Message action opens
   `/threads?dm=<alias>`, and the board's
   `POST /api/threads/<alias>/messages` (`thread_send`) starts the canonical
   home thread inside the enqueue transaction. `/api/threads/<alias>` and
   `/api/work-conversations` then return the same id with the operator
   message and the agent's `turn_result`.

   A Home thread is per agent alias. Plain `cadence send`, dispatch and task
   traffic do not create one; before it exists, read that work through
   `agent show`/`inbox` (and agent↔agent pairs, below). Once the alias has a
   Home thread, its timeline is alias-scoped: a CLI, dispatch, task or peer
   turn that finishes for that alias is noted there too, so the thread can
   show turns that were not started from the board. Full
   conversation-scoped separation — only that conversation's traffic on a
   conversation surface — is CAD-1812's later design and is not implemented
   here. Agent↔agent traffic is a separate pair conversation: ordinary peer
   messages are listed without a filter, while routed `worker_result` and
   notice copies are system traffic and appear only with
   `include_system=true`.
7. Restart, dedup and unknown: `cadence dev reload` resumes registered agents
   on their stored session ids and keeps message ids. Retrying the same
   `--message` id with the same body answers `{"duplicate":true,…}` and starts
   no second turn; different content for a used id is refused. A provider
   connection lost mid-turn records the message `unknown` and fences the
   agent in `attention` with the inspection note; the uncertain turn is never
   replayed. `cadence agent unfence <alias>` is the explicit operator
   reconcile (and resumes unless `--no-resume`). Turnless nudge uncertainty is
   the non-fencing case and never fences the agent.

Native steering on these versions: managed Claude refuses strict nudges;
Codex refuses until its exact version is validated (0.160.0/0.161.0 above);
Pi's generated guard opens turn admission only on Pi 1.0.0, so 1.4.0 serves
ordinary turns and refuses strict nudges (`agent show` distinguishes
`capabilities.native_turn_steering` from `native_turn_steering_enabled`).
Ordinary queued delivery and observation are unaffected.

OMP startup uses its own CLI flags and core-tool names, not Pi's
`--offline`/`--no-themes`/`--no-prompt-templates` or `find`/`ls`. Fresh
workers use `--session-dir` only when no native identity has been recorded.
Once owned, a worker must resume its exact in-worker `session.current` file;
a missing, dangling, outside-worker or mismatched-identity pointer refuses
instead of adopting `session.jsonl` or silently creating another session.
OMP's leading title slot is skipped when reading its actual session header.
The saved working directory must match the requested launch directory:
native OMP relocation is unsupported, so a home-to-lane move refuses rather
than using Pi's fork switches or executing in the old home directory.

The current-session pointer is recorded only once its file exists inside
that worker's directory; a nonexistent initial path is not proof of
resumability. A first session that never materialized cannot be resumed
after shutdown. Preserve its history and refusal; do not delete pointers
or replay uncertain input to obtain a fresh launch.

OMP's ordinary prompt acknowledgment is not completion. Its matching
`prompt_result` reports completed/aborted/error; when `sessionSettled` is
false, the adapter waits for `session_settled` before dispatching the next
durable task. A synchronous response with `data.agentInvoked=false` instead
completes that local command immediately; no model turn or `prompt_result`
will follow. This is command completion, not proof of model inference.
An asynchronous native auth rejection becomes a failed receipt, not a
permanently running task. Strict nudges remain disabled without verified
runtime guard admission. Successful omp replies, authenticated
persistence/restart and peer routing still require end-to-end evidence.

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
provider answer is recorded without overwriting that terminal evidence. A daemon
crash between the durable bind and the provider's reply leaves the row
`submitting`; the boot sweep closes it non-fencing `unknown` (`crash_unconfirmed`)
— maybe applied, never replayed. Events are `native_steer_submitting`,
`native_steer_disposition`, and the existing `nudge_cancelled`. The disposition event's `recorded` flag describes a database
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

Codex steering admits only the verified initialize `userAgent` first tokens
`cadence-agent/0.160.0` and `cadence-agent/0.161.0`. CLI installation, a static
registry capability or a compatible-looking schema alone does not enable it.
Other versions, including development suffixes, retain ordinary queued delivery
but refuse strict nudges until their exact-turn behavior is revalidated.

Codex validates `expectedTurnId` inside the app-server. Wrong or missing turn ids
in a successful reply are unknown, not accepted. The adapter releases
reader-shared locks before waiting, so a quota notification cannot block receipt
processing. There is no steer-to-start/follow-up fallback. Between the provider's
`turn/start` acknowledgement and the durable `running` write the daemon sees no
steerable turn, so a nudge landing in that gap closes `skipped_inactive` — the
turn was live but the window is conservative, never a silent queue.

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

The independently authored Codex 0.161.0 refusal check uses a newly owned
ephemeral thread and the installed CLI's normal authentication:

```sh
python3 tests/codex_native_steering_acceptance.py --run \
  --scratch "$(mktemp -d /tmp/codex-steer.XXXXXX)"
```

It requires stale-target refusal while another turn is active, with a valid
current-turn positive control and no stale guidance in the successor's events.
Without `--run`, it performs only initialization, not model inference.
Direct runtime probes are not Cadence end-to-end evidence.

CAD-1583 separately exercised the lane binary in temporary Cadence state:
the old admission refused 0.161.0; the updated adapter accepted guidance while
the original kickoff remained running; its matching result contained both
the original objective and guidance markers. An ordinary follow-up completed
separately. The tested model was `gpt-6.1-sol`; no existing sessions or
production daemon were used. Ticket artifacts retain source/binary hashes,
protocol and adapter evidence, and cleanup. Future runtimes require fresh
validation; this is not certification of every Codex version.
