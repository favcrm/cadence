# Offline local result custody

`remote_result_outbox` provides CAD-682's immutable local custody. CAD-700 adds
an explicit offline CLI wrapper. CAD-716 sends a retained command to AOS-74's
board-host queued-custody route using the issuer-bound child bearer in a private
enrollment record.
There is no daemon integration, automatic credential resolver, assignment
transport or applied-task witness. CAD-675 retains those integration gates.
The publication Outbox UI is a separate feature.

## Explicit offline CLI

Retain a completed result envelope from stdin using an explicit original pin:

```sh
cadence remote result retain --outbox-dir /absolute/private/results \
  --org org-1 --audience https://board.example.invalid \
  --subject subject-1 --agent agent-1 < result.json

cadence remote result pending --outbox-dir /absolute/private/results \
  --org org-1 --audience https://board.example.invalid \
  --subject subject-1 --agent agent-1
```

Every selector is required; no daemon state, HOME/XDG, org default or stored/env
credential supplies any selector. `--state-dir` is refused. These verbs dispatch
before daemon/config/credential resolution and start no network, browser or
provider. An inherited agent label or token supplies no extra authority.

Retain reads at most 64 KiB plus one detection byte and validates the pin and
complete strict envelope before creating/opening custody. It prints only the
committed local_pending receipt: commandId, digest, storedAt and the original
destination. An exact retry returns the same receipt; conflicting content or pin
refuses without replacing bytes. No result text or credential-shaped extra field
is echoed in output or parse errors. Text belongs in stdin, never a CLI argument.

Pending requires the directory and results.sqlite3 to exist, filters the exact
pin, and prints at most 128 receipts with that same metadata and no result text.
It does not initialize missing custody. Protected open may create its init lock
or recover a valid hot journal, so inspection is **not universally read-only**.
Unsafe/foreign database and journal protections and same-UID path race limits
below continue to apply. There is no deletion, retarget, applied receipt,
authenticated enrollment or local task mutation in these offline verbs.
Caller pins remain structural metadata; reported task/head remain assertions.

Hosted command and destination IDs use ASCII letters/digits/underscore/dash,
1–200 bytes, matching AgenticOS's shared hosted-id contract. Audiences retain
exact canonical HTTPS origin rules. Unsupported issuer IDs are refused, not
normalized or truncated. Issuer-bound identity checks and credential rotation
remain CAD-675/AOS-62 integration dependencies.

The library interface is `ResultCommand::parse_json`, `DestinationPin::new`, and
`ResultOutbox::open`, `enqueue`, `pending_for`, `get`, and `deliver_with`. The
caller supplies an explicit absolute directory and destination when retaining.
There are no environment defaults, deletion or retarget methods. The directory's parent must
already exist; creation affects only the requested leaf and its offline files.

An enqueue returns a stable **local_pending** receipt after the SQLite commit.
This means local bytes were retained, not that a server authenticated an agent,
accepted a command or applied a task transition. Every returned row carries its
original organization, exact gateway origin, subject and agent pin. A later org
selection cannot change it. `pending_for` returns only the requested original
pin; the sender uses that stored destination and requires genuine enrolled
child authority rather than reinterpreting labels as membership or reusing the
global default.

Pins are structurally checked metadata, never authenticated identities. They
contain no dedicated credential/token fields. Result text remains opaque and
can contain sensitive user content; private files are not encryption or a
guarantee that the text contains no secret. Error messages and command Debug
output do not echo result text.

## Explicit hosted queued-custody send

First bootstrap the private [issuer-bound enrollment](REMOTE-AUTH.md) using the
AgenticOS service credential. Then send with that enrollment directory:

```sh
cadence remote result send --outbox-dir /absolute/private/results \
  --command-id command-1 --enrollment-dir /absolute/private/hosted-worker
cadence remote result status --outbox-dir /absolute/private/results \
  --command-id command-1
```

The private enrollment record is the **only** send credential source. Stdin,
argv, `CADENCE_TOKEN`, `agc_`, config, environment, selected-org and browser
state do not supply a send bearer; missing, expired or mismatched enrollment
leaves `local_pending` unchanged. The independent operator-created trusted
issuer pin is rechecked on use. The bound organization, exact board origin,
subject and agent must all match the original outbox destination before the
send callback or any network I/O. A caller cannot use `--org` or `--audience`
to assert destination authority. The sender does not print the bearer, command
text, HTTP error body or response URL.

The sender loads the original canonical JSON and exact organization/HTTPS board
origin from protected local custody by command ID. It validates the stored pin
against the issuer enrollment while holding the enrollment read lock through
the network exchange. A concurrent remove or renewal orders after that send.
The sender then posts to
`/__platform/hosted-cadence/<organizationId>/results`, disables redirects and
ambient proxies,
bounds the entire exchange to 12 seconds and the response body to 4 KiB. Only
HTTP 202 with `{ "ok": true, "receipt": ... }`, no extra fields, state `queued`,
the original command ID and SHA-256 digest, and safe increasing receipt times
can advance local custody to `remote_queued`. All other responses and transport
errors leave the command locally pending for an explicit retry. The original
queued receipt is stored transactionally and survives restart; concurrent or
repeated matching responses return it, while a conflicting receipt is refused.

`status` reads only local custody. It reports `local_pending` or
`remote_queued` plus `applied_unknown`; it makes no HTTP request, wakes no
runtime and never claims task application. A 202 receipt proves queued server
custody only. Network failure after a server commit is **uncertain** until an
explicit retry obtains the same queued receipt. There is no cloud polling,
application acknowledgement, restore-safe witness or cleanup here.

## Envelope and digest

The codec consumes the strict `hosted-cadence-result.v1` envelope from AOS-64
PR130 at `8312b78b83af51cb2ea025914004efd7c830e917`. Version/kind and all fields
are required; extra fields and duplicate JSON keys are refused. Command and
destination IDs use the shared hosted contract's ASCII letters/digits/underscore/dash
bound of 1–200 bytes. Larger issuer IDs are unsupported, never truncated or
mapped to invented identities.

Revision matches decoded positive safe-integer Number semantics: `1.0` and
`1e0` become canonical integer `1`, and values above 9007199254740991 fail.
Reported heads are exactly 40 or 64 lowercase hex characters. Rust/serde rejects
lone escaped surrogates. Complete canonical JSON is bounded to 32 KiB; raw
parsing also has a separate 64 KiB resource bound, so excessively padded or
escaped spellings can fail even when their canonical form is smaller.

The canonical field order and UTF-8 digest are fixed by this escaped/emoji
fixture, independently shared with the server parser:

```text
{"version":"hosted-cadence-result.v1","commandId":"cmd-golden","kind":"agent_result","assignmentId":"assignment-1","taskId":"task-1","taskRevision":1,"turnId":"turn-1","reportedHeadSha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","text":"Done \"quoted\"\npath\\file\t😀"}
```

SHA-256: `ddc2a7269a5b94cd80c5e59e30068a96645864de94413b8833f0a3f47d4da7b6`.
Destination identity is stored separately and compared exactly, rather than
changing that wire digest. The same command ID, complete canonical payload and
original pin returns the first receipt. A changed task/revision/head, dispatch,
turn, text or any destination component conflicts without replacing bytes.

Gateway origins are checked conservatively: lowercase ASCII DNS labels,
canonical dotted IPv4 or compressed IPv6, HTTPS and an optional canonical
non-default port. Credentials, wildcard hosts, paths, query/fragment, default
443, uppercase hosts and redundant port zeros are refused without normalization.
Hex/octal-style IPv4 aliases, DNS labels with hyphen edges and IPv4-mapped
IPv6 spellings are also refused. Unsupported DNS spellings must be resolved by
the trusted board-origin contract. This structural check neither contacts nor
trusts a gateway.

Fields describing task/dispatch/turn/head are client assertions. This library
does not verify current assignment, endpoint generation, role, revocation or
commit existence. Task reopen can reset a revision; the retained assignment ID
must later be checked against the current dispatch message. The board HTTP
gateway parses original JSON before forwarding it through RPC, whose string
serialization can replace lone surrogates before the destination parser sees
them. This offline codec proves no HTTP authorization boundary.

## Files, bounds and recovery limits

The separate `results.sqlite3` file never opens the daemon Store or runs its
migrations/recovery. A private initialization lock coordinates independent
openers, refusing contention after five seconds. Descriptor opens use
NOFOLLOW and NONBLOCK before checking file type, so FIFOs cannot hang validation.
The owned directory must be exactly 0700 and the database/lock files
0600 regular files, without symlinks or extra hard links. Existing unsafe paths
are refused, not repaired. Existing database headers must identify the offline
application/version and rollback-journal format **before** a writable SQLite
open. SQLite specifies big-endian user version at offset 60 and application ID
at offset 68 in its [database header](https://www.sqlite.org/fileformat.html#the_database_header).
An existing file also receives a read-only preflight requiring the exact v1 or
v2 table DDL (exactly the library’s own spelling, without whitespace folding),
constraints and implicit primary-key indexes, with no extra tables, views,
indexes or triggers, before any writable SQLite open. A valid v1 file is
upgraded transactionally to v2 by adding the queued-receipt table; original
pending rows are retained.
If that preflight specifically encounters [SQLite READONLY_ROLLBACK](https://www.sqlite.org/rescode.html#readonly_rollback), a hot
rollback journal needs recovery to inspect the schema. Existing journal paths
receive the same private-file checks; WAL/SHM sidecars are unsupported and refused. A private disposable
copy, in a scratch directory explicitly created with 0700 permissions, of the
owned database and journal receives recovery and the same schema
check first; each copied file is capped at 16 MiB and checked as an owned 0600
regular file with one link. Failed preflight retains original database and
journal bytes. Successful preflight allows SQLite to recover the original,
whose exact schema is checked again before the returned handle can enqueue.
Copy/recovery/schema failures retain originals. The preflight is not an
authoritative data snapshot or an atomic defense against a hostile same-UID
writer. Supported concurrent enqueue never changes the schema; contention can
still require refusal and a later retry.
These public identifiers prevent accidental foreign-file use; they are not
cryptographic identity or protection against a hostile owner of the same UID.

Transactions use DELETE journal mode, FULL synchronization, an immediate writer
transaction and a five-second busy timeout. Duplicate lookup, global per-file
capacity check and insert share that transaction across independent handles.
The file retains at most 128 pending entries. New IDs at capacity fail; exact
duplicates still recover their receipt. No expiry, garbage collection or
automatic cleanup removes acknowledged local bytes. Altered/corrupt rows are
refused and retained for inspection, not reported as valid custody.

SQLite's [synchronous setting](https://www.sqlite.org/pragma.html#pragma_synchronous)
requests durable commit behavior, subject to the filesystem/device honoring
flushes. Commit/reopen and concurrent-handle tests do not certify host power
loss or storage hardware. An interrupted initial creation can leave a partial
file that this foundation refuses for explicit inspection; it does not silently
delete or recreate it. Same-user replacement of directory ancestors or files
between filesystem checks and SQLite open remains outside this safety boundary.
The path checks and NOFOLLOW flag do not claim a race-proof filesystem sandbox.

An older local backup can omit later pending work. Local receipts alone cannot
prove external application or rollback detection. Application witnesses and
restore reconciliation remain separate promises;
there is no applied state or exactly-once external-effect claim here.

## Verification scope

`tests/remote_result_outbox.rs` uses temporary local files and independent SQLite
handles for codec/golden, malformed/forged fields, exact byte limits, reopened
receipts, identical/conflicting concurrent retries, capacity races, immutable
destination selection, corruption retention, privacy/symlinks and unchanged
foreign-file bytes/SHA, copied-header wrong schemas/triggers, bounded lock
contention and FIFO refusal, actual abrupt-child-exit rollback recovery, and
hot-journal foreign-schema byte preservation. It starts no daemon or network client. Tests were written
first in source; red execution was unavailable because the external author had
no admitted native build recipe. Compilation and behavioral execution belong to
CI plus independent review, not the standalone formatter.

`tests/remote_result_cli.rs` exercises the actual freshly built CLI with isolated
short temp roots: no-HOME and poisoned profile/org/auth defaults with an absent
daemon and inherited master label, separate-process metadata inspection, exact
retry bytes/receipt, each destination conflict, altered text/head, canonical
numeric retry, malformed/oversized stdin before creation, missing custody,
foreign/symlink database/journal unchanged bytes, and concurrent process retry.
`tests/remote_result_sender.rs` checks the exact pinned body/URL, strict 202
receipt, altered command and pin conflict, wrong credentials/statuses, receipt
reopen, v1 migration, concurrent duplicate send and actual CLI enrollment/status
behavior, including zero connection to an untrusted outbox origin.
`src/cli/remote_result.rs` also drives the production HTTP helper
through a loopback server to inspect its actual Authorization header, content
type and canonical bytes, matching 202 receipt, redirect handling and response
bound. This local transport fixture uses HTTP because CI has no publicly trusted
local TLS name; the production pin validator requires HTTPS and the transport
disables ambient proxies. Live HTTPS issuer/enrollment and gateway activation
remain CAD-675 end-to-end gates.
