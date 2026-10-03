# External agents doing social management

An operator-provisioned person or AI agent off-host can do Social
Content work through Cadence: the operator creates quoted runs the
agent executes, the operator reviews receipts, and every send stays
a human approval. This guide is the one written path; inventing
another access shape (shared credentials, the operator secret, a
pasted provider token) is not a shortcut.

## Prerequisites

- A build that includes CAD-777 (device-grant board sign-in) for V2;
  V1 works on any recent build.
- The daemon on its host, loopback-bound, with an enrolled
  `agenticos_external` connection (`provider.read`/`provider.draft`)
  and an approved Social Content catalog install.
- Destinations come from owner-authorized discovery, never a typed
  handle or pasted URL.

## V1 access: SSH shell + joined agent identity (works today)

SSH or tailnet gives the agent a shell on the host — nothing more.
Agent identity comes only from joining as a Cadence agent
(`cadence join <pm> <provider>`) or an enrolled endpoint, so every
action carries its alias. The board stays operator-only: agents work
through the CLI; the human approves in the board. The daemon socket
stays local; no new listener. No shared credentials, no operator
secret, no `--token-stdin` values ever leaving your own shell.

## Teams and grants: per run, through the owner group

There is no standing "agent may do social" permission. For catalog
installs the team rides the run inputs, validated at create time
against one owner group:

```sh
cadence agent register <pm> --provider <p> --endpoint <k> --role pm
cadence agent register <writer> --provider <p> --endpoint <k> \
  --param upstream=<pm>
cadence agent register <reviewer> --provider <p> --endpoint <k> \
  --param upstream=<pm>
```

```json
{
  "subject": "Short label",
  "source": "Pasted facts, exact names, prices and disclaimers",
  "writer": "<writer>",
  "reviewer": "<reviewer>"
}
```

```sh
# The operator creates the run; the team rides the inputs and is
# validated at create time against one owner group.
cadence app run create <install> --workflow instagram \
  --inputs ./inputs.json --request-id <unique> --owner-pm <pm>
```

Create refuses workers outside the owner group, non-distinct
writer/reviewer, and unenrolled providers (PTY and remote endpoints
are unsupported for runs). The agent never creates runs — it executes
its assigned produce/review steps from the kickoff it is sent.
Installing (or updating) the app approves its local capabilities; it does not derive
team grants. (`app set-team` and `app approve` with grant derivation
are the legacy project-app verbs — different syntax, different path.)
Writer and reviewer must differ; the same alias never holds both roles
on one run. Agents get no admin verbs: no `connection
create/rotate/revoke`, no approvals, no grant widening.

## Allowed vs forbidden

An agent may: execute its assigned produce/review steps (including
assigned-turn artifact and capability reads with its message and turn
token), report results.

An agent may never: create a run, approve its own (or any) run,
release a send, select or alter a destination, mint a session,
rotate/revoke anything, or approve a schedule. Run creation,
connection, catalog and install reads, and receipt fetches need
operator proof — they are the operator's checks, not the agent's.
Sends execute only after the operator's digest-pinned approval in the
board (CAD-771); revocation closes affected waiting effects with
reason `grant_revoked` through the custody lifecycle.

## Pre-flight: prove attribution before first work

The operator runs these from their own shell, before the agent does
real work:

1. Confirm the alias, group and endpoint:
   `cadence agent show <alias>` — role, provider, upstream as
   provisioned.
2. From the agent's account, attempt an operator-only call — it must
   be refused, and the refusal must name the **agent alias**, never
   `operator`.
3. Confirm `cadence ui sessions` (operator-only) shows no device or
   link session for the alias.

Fail-open attribution — anything attributed to `operator` that is
not the operator, or any operator-only success — stops the
onboarding. That is a security finding, not a docs gap.

## V2: device-grant sign-in (issuer side pending AOS-97)

The human operator signs into the board without SSH. Agent access
stays CLI-shaped. Unchanged: alias-carried actions, per-run teams,
human-approved sends.

Configure it once, from your own shell, on the daemon:

```sh
cadence ui device-login set --issuer https://your-agenticos-api.example \
  --org ws_company --subject <your-subject-id>
```

Supply issuer, org and at least one subject together, or none of them.
`--subject` repeats, once per operator who may sign in remotely. The
write goes through the operator-secret RPC — run it from your own
shell, not an agent pane. (`cadence ui start`/`run` still accept the
`--device-login-*` flags as a thin client over the same `set`, env
`CADENCE_DEVICE_LOGIN_ISSUER` / `_ORG` / `_SUBJECTS` included; the
daemon's store is the persistence, not `ui.json`.) The config applies
live to a running board — `cadence ui device-login show` prints it,
`clear` removes it — and survives board restarts. It is the operator's
allowlist: any other verified workspace member is refused
`device_subject_not_allowed`. Find your subject id with
`cadence auth status` (`principal.subject_id`); a refused sign-in
also names the subject it saw. If the configuration is cleared or
changed while your grant is being verified, the sign-in is refused
(`device_authority_changed`); start it again.

On the board, open the header's **Sign in** menu and choose **Sign in
with AgenticOS**. The board shows a user code and an approval link,
then polls until the grant is approved, denied or expired. A remote
session lasts 12 h idle and 24 h at most. Revoking the grant at the
issuer does not end a live board session early; sign it out from
`cadence ui sessions`. Issuing the grant still needs the issuer's
device flag (AOS-97).

Explicit non-goal of this guide: scoped API/MCP agent bearers for
headless agents. That is a separate ticket with its own adversarial
tests — this guide's agents are interactive CLI users.

## Revocation

Remove the alias (or its group membership), revoke affected
approvals, close its access. Verify as the operator: its sessions
are gone from `cadence ui sessions`, its waiting effects close with
reason `grant_revoked`, and its next call is refused as an
unknown/ungranted caller.
