# Operator guide: SOP workshop (manual use)

This is a manual, explicit-load task method. It is not an installed profile,
automatic discovery, persistence engine or activation. Managed Pi workers launch
with skill discovery disabled, so the worker must be told to read the file.
Use an already registered, authorized worker by its actual alias; do not guess
aliases, register a substitute or invent commands.

The method is in [`SKILL.md`](SKILL.md). Domain rules stay in their own sources
(`skills/cadence/SKILL.md`, `skills/business-documents/SKILL.md`,
`skills/agent-handover/SKILL.md`).

## Give the worker a task

Create or use one Cadence issue for the procedure. Keep the requirement notes,
drafts and the proposed SOP as issue artifacts.

Send a task file that names the issue, the goal, and the path to the method.
If the worker cannot read this repository, paste the contents of `SKILL.md`
into the file instead.

```text
Work on issue CAD-1234. Goal: set up a minimal SOP for <procedure>. Follow
skills/sop-workshop/SKILL.md (paste its contents if this path is not accessible).
Ask only decision-changing questions, propose the smallest SOP, produce a bounded
draft, then run feedback rounds as needed. Label each claim by evidence, keep draft
corrections, SOP revisions and scope changes separate, and offer any reusable
suggestion as this-run-only, workspace SOP or candidate general lesson. Do not
send, submit, purchase, record approvals or self-install.
```

Dispatch it with the supported durable-message command. Use the actual alias of
a registered worker; `--reply-to` names the PM alias for the report:

```sh
cadence send "$AGENT_ALIAS" --file ./sop-workshop-task.md --reply-to "$PM_ALIAS"
```

`cadence send` enqueues a message. It does not grant file access, create a ticket
or guarantee that the worker can read this repository or issue artifacts. Confirm
the worker's access before relying on a path.

## Record outputs

Store the outputs as issue artifacts, one distinct filename per revision, and say
in a comment which revision is authoritative:

```sh
cadence issue attach CAD-1234 ./sop-draft-r2.md
cadence issue comment CAD-1234 --text "Authoritative SOP: sop-draft-r2.md. Status: proposed, not owner-confirmed."
```

`attach` stores a basename-only file with a size cap; it does not redact or
classify data. Use `cadence help issue attach` and `cadence help issue comment`
for the installed CLI's exact options.

## Limits

The worker's report is a claim until you check the artifacts. A reusable
suggestion needs the owner's workspace decision or the existing reviewed
memory/skill process. Do not treat a Markdown SOP, a conversation confirmation,
a successful receipt or the absence of complaints as activation, approval or
proof of general benefit.
