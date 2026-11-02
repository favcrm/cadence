# First run: scope, sign-in, billing and the handover (CAD-1922)

`cadence setup` is the idempotent first run: it creates what is missing —
state dir, tracker, skill, daemon, board — and reports the provider CLIs,
the master agent and the operator login. Anything already present is
reported `ok` and left untouched.

On a first run in a real terminal it asks one question first, then hands
over four facts and exactly one next command. The home and credential
paths it uses are the ones the Cadence home resolver returns; setup only
creates at those paths and never moves, copies or switches an existing
installation.

## The one question

```
Cadence first run — where does your work live?
  Enter  sign in to hosted AgenticOS (I already have a workspace)
  1      stay on this machine — no account, no browser
  2      create a hosted workspace in the browser
```

- **Enter** opens the same hosted device grant `cadence login` uses.
- **`2`** opens the same registration grant `cadence register` uses; the
  browser creates the workspace and continues to the normal consent page.
- **`1`** stays local: no login, no browser, no checkout.

At most **one** device grant is opened per run. Registration is a route of
this one question, not a second grant after a failed sign-in. Both hosted
routes (**Enter** and **`2`**) request the optional owner-only
`billing.read` on that same consent — never `billing.cancel`; **`1`**
requests no scope and opens nothing. Nothing is signed in for a managed
agent (`CADENCE_ALIAS`, `CADENCE_GRANT_TOKEN`) or inside a `cadence
dev`/sandbox instance.

The question is asked only when this run's own state dir and tracker are
still absent and no hosted login or saved org exists. It is read from a
terminal **or a pipe**: one line, where an empty line is hosted sign-in,
`1` stays local and `2` creates a workspace; EOF with no line stays local.
A pipe gets the question on stderr, so the run's own text output is never
mixed with it. `--json` never reads the answer.

| Flag | Effect |
| --- | --- |
| `--local` | Never ask: stay on this machine (also the answer for a pipe with no line). |
| `--hosted` | Sign in to an existing hosted workspace, no question. Refused with `--json`. |
| `--register` | Create a hosted workspace, no question. Refused with `--json`. |
| `--no-open` | Print the approval URL and code instead of opening a browser. |
| `--json` | Machine records only: no prompt, no sign-in, no billing call. |

`--local`, `--hosted` and `--register` skip the stdin read entirely, for
automation that must not wait on a pipe. `--json` combined with
`--hosted` or `--register` is refused before any credential is read, any
grant is opened or any check is applied: the machine path never starts a
browser approval, so the contradictory pair is an error rather than a
silent sign-in. Use `--json` alone (stays local) or the sign-in flag
without `--json`.

A first run records the workspace binding without moving the saved default
(CAD-1125), so ambient local stays the default and the next command names
the workspace explicitly.

## What a hosted run does not start

The daemon and the board are local services. A run whose scope is hosted —
and a run whose requested browser approval did not complete — reports them
without starting either, so signing in never starts a local board. Only a
run that chose this machine starts the daemon and the board, exactly as
before. `cadence ui start` is the explicit command to start the board
later.

## The plan

After a verified hosted sign-in, setup reads
`GET <board>/__platform/hosted-cadence/<workspace>/billing/offer` with the
saved human login — read only. Every plan, price, limit, trial duration,
eligibility and term printed comes from that response; nothing is
hard-coded and nothing is guessed.

Trial and paid subscription are separate states:

| State | Meaning |
| --- | --- |
| `skipped` | A subscription is active: `No new plan needed.` |
| `trial_active`, `trial_eligible`, `trial_used`, `trial_expired`, `trial_failed`, `trial_unknown` | The cardless registration trial, exactly as the server policy reports it. It never opens a Checkout and never converts to a paid plan by itself. |
| `checkout_pending`, `checkout_open`, `checkout_completed`, `checkout_unknown` | An outstanding Checkout. `open` carries the one URL to resume; `completed` is Stripe-complete but not reconciled; `pending` and `unknown` fence a new Checkout. No second Checkout is started. |
| `checkout_settled` | A Checkout completed and was reconciled, so it fences nothing. Whether a new paid action may start comes from the offer's own `available`/`blocked`; the terminal still opens nothing. |
| `past_due`, `subscription_unknown` | Not an active subscription. `subscription_unknown` is never reported as definitively inactive. |
| `none` | No hosted workspace on this machine: nothing is billed. |
| `unknown` | Not read by this run. Never a claim that there is no plan. |
| `unavailable` | The server refused or could not answer; the server's own code is reported. |

The independent components use their own names: `plan.subscription` is
`subscription_active`, `subscription_past_due`, `subscription_inactive`,
`subscription_none` or `subscription_unknown` (never reported as inactive
when unknown); `plan.checkout` is `checkout_pending`, `checkout_open`,
`checkout_completed`, `checkout_settled`, `checkout_unknown`,
`checkout_none` or `checkout_expired`; `plan.trial` is the `trial_*` state.
The summary `state` above keeps the previous precedence for the one plan
fact.

The Checkout operation is read before the offer's `available`/`blocked`
fields: those describe only whether a *new* Checkout may start, and a
fenced operation legitimately reports `available: false` with its own
block reason. Only `none`, `expired` and `settled` consult availability.
Trial, paid subscription and Checkout are reported independently: the
handover carries each as its own `plan.trial` / `plan.subscription` /
`plan.checkout` fact, and the printed plan line names all three, so an
active trial is never hidden by an open Checkout or an active subscription
(and vice versa). Trial eligibility (`eligible`, `used`, `unproven`,
`unavailable`, `unknown`) is reported with the server's own reason;
`eligible` is a decision this read can make, not a promise that the
registration door will grant the trial, and no eligibility value is turned
into a duration, cap or expiry. An open Checkout is reported with its
server URL; the terminal does not tell the owner to cancel it, because no
user-facing cancellation control is confirmed yet.

The terminal never opens a Checkout, never cancels one and never asks for a
card, CVC, OTP, password or provider secret. Starting a paid plan is a
separate, explicit action in the workspace dashboard; card entry happens on
Stripe.

Billing is an owner-only capability (`billing.read`). The first run's one
sign-in/registration grant requests it as an optional scope alongside the
ordinary CLI/wiki scopes — `billing.cancel` is never requested, and the run
never opens a second grant or a second login. The consent page leaves
owner-only scopes unchecked: an owner who declines `billing.read` still
signs in and the plan reports `unavailable`; a member cannot consent it, so
a member's approval returns the same narrowed grant and their login still
works. A grant without it reports `unavailable` with the server's
`capability_not_consented` — that is not a reason to sign in again, and
setup never reopens a login, reuses another credential or invents consent
to obtain it. Ordinary `cadence login`/`cadence register` and
`--cloud-agent` logins keep their existing scopes; only this run's explicit
consent can add `billing.read`, and existing saved sessions are never
silently reauthenticated. A grant issued before billing existed keeps
working for every other verb. The plan and any Checkout are shown in the
workspace dashboard.

## The handover

```
first run
         scope        local | hosted
         sign-in      signed out, or the verified slug and endpoint
         plan         none | trial/checkout/active state, or unknown/unavailable
         board        the board URL this run started or found
         next         exactly one command
         live         the live instruction
```

The live instruction says what to ask for, says not to rerun setup, open a
second login or checkout, or type a card, and names the briefing's own
report route: `cadence message result <id> --token <turn_id> --text '...'`
for a token reporter, the final turn text for a turn-result reporter.
`cadence self` and `cadence agent list` remain the live truth; the briefing
stays the durable snapshot. `cadence status` reports work, not billing.

A sign-in this run verified is `verified`. A login this machine already
holds is `saved`: the credential file is named, never read, so it is not
verified and may be expired. A failed approval is `failed` with the
issuer's own message; a workspace the browser created, if any, is not
bound to this machine because no credential was saved. A store that cannot
be inspected is `unknown`, never `signed out`.

`scope` is the scope this run selected, not what the credential store
happens to hold: `1`/`--local` stays local even when a hosted login is
saved, and a failed approval bound nothing, so it stays local too. Only
this run's verified hosted sign-in makes the scope hosted.

No outcome proposes another approval — the live instruction already says
not to open a second login, and after an interruption (denial, expiry,
transport failure, a grant that did not match) an approved credential may
already exist server-side. The one next command is therefore always a
truthful read of this machine, in the scope this run selected: a local
run hands over `cadence status --state-dir <this run's state dir>` (the
pin keeps a saved remote org from retargeting it), and a hosted run with
no inherited pin hands over `cadence status --org <workspace>`. A hosted
run from a pinned shell — where `--org` would refuse a conflicting
destination — also hands over the local, state-pinned read.

## Records and exit codes

`setup --json` keeps its existing records: one `{check, status, detail,
fix}` object per line, then the `scope` row. The `scope` row additionally
carries `handover` (`scope`, `sign_in`, `plan`, `board_url`,
`board_running`, `hosted_board_url`, `next_command`, `instruction`); no
existing field or line changes. `plan` keeps its `state`, `detail`,
`checkout_url` and `code`, and adds `trial`, `subscription` and `checkout`
as independent `{state, detail}` facts when the offer was read (absent when
it was not, never a default). `--json` never prompts, never signs in and
never calls the billing API. A pipe is asked the one question on stderr —
an empty line signs in, `1` stays local, `2` registers, and EOF with no line
stays local — so the run's own output is never mixed with it.

Exit 1 when a check failed, and when a requested browser approval did not
complete. `missing`, `unknown`, `unavailable` and a plan that was not read
are facts, not failures.
