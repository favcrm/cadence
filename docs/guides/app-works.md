# Works content for an installed app

Works is the host-owned document, deck and sheet store for an installed app
screen. An approved Works screen reads and edits Works items, asks an
independent person for review, and receives an export receipt for the approved
current revision. The host, not the app, decides who may approve or export.

## Operations

Declare only the operations your screen needs in `screens.json` under the
`screen-records.v1` host contract. Undeclared operations are refused.

| Channel | Operation | Purpose |
| --- | --- | --- |
| CALL | `works.query` | List items with filters |
| CALL | `works.get` | Read one item |
| CALL | `works.create` | Create a doc, deck or sheet |
| CALL | `works.patch` | Edit title, content or tags at `if_revision` |
| CALL | `works.trash` | Move an item to trash at `if_revision` |
| CALL | `works.restore` | Restore a trashed item at `if_revision` |
| CALL | `works.request_review` | Ask for review of one revision |
| SLOT | `works.review` | Host-confirmed reviewer decision (`approve` or `changes`) |
| SLOT | `works.export` | Host-confirmed export receipt for the current approved revision |

CALL operations are operator-only. Every Works operation is admitted only for a
verified operator session on the installed screen. Screen arguments carry
selectors and content only. They never carry an installation, context, actor,
reviewer identity, session, role or approval.

Export is receipt-only: `{id, revision, format, digest}`, where `digest` is
the approved revision's content digest. Rendered file generation is not part
of this door.

## Review rules

- A review needs a pending current revision.
- The author and the reviewer must be two distinct verified humans. A second
  session of the same human is refused as self-review.
- Unverified authors and unverified reviewers are refused.
- Registered agents, detached callers and non-operator members are refused.

## Export rules

Export succeeds only for an approved revision that is still current and not
trashed. Export before approval, after `changes`, after a later edit, for an old
revision, and for trashed content are refused with `needs_review`.

## Confirmation cards

Review and export use host confirmation cards. The card shows the authoritative
title, kind, revision and digest that the host re-read. Preparing a card changes
nothing. A confirmation can be used once, expires, and refuses if any fact it
bound has changed since preparation.

## Limits

Works create and patch bodies are bounded to 80 KiB, which fits the screen
bridge. Oversized requests refuse before any write.

## Refusals

Refusals keep their store code through the bridge:

- `conflict`: stale `if_revision`, or a changed payload under the same key.
- `not_found`: the item is missing or outside the installation and context.
- `forbidden`: the identity fails the review rules above.
- `invalid`: the payload does not match the declared shape.
- `needs_review`: export was requested before an approved current revision.

## Native operator CLI

`cadence app works` reaches the same store through the native `app_works_*`
methods. It needs a running daemon and an operator connection.

Every verb takes an explicit installation and context. No verb accepts an
actor, reviewer, on-behalf-of or session flag.

```sh
cadence app works query <install_id> --context-id <ctx> [--text ...] [--kind doc|deck|sheet] [--review ...] [--owner ...] [--sort ...]
cadence app works get <install_id> --context-id <ctx> --id <item>
cadence app works create <install_id> --context-id <ctx> --body item.json --idempotency-key <key>
cadence app works patch <install_id> --context-id <ctx> --id <item> --if-revision <n> --body patch.json --idempotency-key <key>
cadence app works trash <install_id> --context-id <ctx> --id <item> --if-revision <n> --idempotency-key <key>
cadence app works restore <install_id> --context-id <ctx> --id <item> --if-revision <n> --idempotency-key <key>
cadence app works request-review <install_id> --context-id <ctx> --id <item> --revision <n>
cadence app works review <install_id> --context-id <ctx> --id <item> --revision <n> --decision approve|changes [--note ...]
cadence app works export <install_id> --context-id <ctx> --id <item> --revision <n> [--format <label>]
```

- `create` reads `{kind, title, content, tags?}` from a JSON file of at most
  80 KiB.
- `patch` reads `{operations: [...]}` from a JSON file of at most 80 KiB.
- `export` omits `format` when you do not pass it; the daemon defaults to
  `html`. The format is a bounded label recorded on the receipt; it does not
  select a renderer in this door.
- Run `cadence app works <verb> --help` for the exact flags of each verb.

A local CLI review without a verified human proof is refused. Two operators
with separate verified sessions are required for an independent review.

## Separation from other approvals

App catalog approval and workflow run approval do not approve a Works revision.
Only a distinct verified reviewer's `approve` on the current pending revision
does. Installing or upgrading an app never grants revision approval.
