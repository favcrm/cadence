# Offline local result custody

`remote_result_outbox` is a dormant library foundation for CAD-682. It has no
CLI, daemon, HTTP, credential, org registry or network integration. CAD-675
still requires the real issuer/enrollment, assignment transport and restore-safe
cloud application protocol. The publication Outbox UI is a separate feature.

The small interface is `ResultCommand::parse_json`, `DestinationPin::new`, and
`ResultOutbox::open`, `enqueue`, `pending_for`. The caller supplies an explicit
absolute directory and destination. There are no environment defaults, send,
acknowledgement, deletion or retarget methods. The directory's parent must
already exist; creation affects only the requested leaf and its offline files.

An enqueue returns a stable **local_pending** receipt after the SQLite commit.
This means local bytes were retained, not that a server authenticated an agent,
accepted a command or applied a task transition. Every returned row carries its
original organization, exact gateway origin, subject and agent pin. A later org
selection cannot change it. `pending_for` returns only the requested original
pin; a future sender must use that stored destination and obtain fresh genuine
authority, not reinterpret labels as membership or reuse the global default.

Pins are structurally checked metadata, never authenticated identities. They
contain no dedicated credential/token fields. Result text remains opaque and
can contain sensitive user content; private files are not encryption or a
guarantee that the text contains no secret. Error messages and command Debug
output do not echo result text.

## Envelope and digest

The codec consumes the strict `hosted-cadence-result.v1` envelope from AOS-64
PR130 at `8312b78b83af51cb2ea025914004efd7c830e917`. Version/kind and all fields
are required; extra fields and duplicate JSON keys are refused. Command and
destination IDs use the current inbox's ASCII letters/digits/underscore/dash
bound of 1–128 bytes. Larger issuer IDs are unsupported, never truncated or
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
IPv6 spellings are also refused. Unsupported DNS spellings must be resolved by the future trusted gateway
contract. This structural check neither contacts nor trusts a gateway.

Fields describing task/dispatch/turn/head are client assertions. This library
does not verify current assignment, endpoint generation, role, revocation or
commit existence. Task reopen can reset a revision; the retained assignment ID
must later be checked against the current dispatch message. A future HTTP
gateway must parse original JSON before forwarding it through RPC, whose string
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
An existing file also receives a read-only preflight requiring the exact v1
table constraints and implicit primary-key index, with no extra tables, views,
indexes or triggers, before any writable SQLite open.
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
prove external application or rollback detection. Future cloud queued receipts,
application witnesses and restore reconciliation remain separate promises;
there is no applied state or exactly-once external-effect claim here.

## Verification scope

`tests/remote_result_outbox.rs` uses temporary local files and independent SQLite
handles for codec/golden, malformed/forged fields, exact byte limits, reopened
receipts, identical/conflicting concurrent retries, capacity races, immutable
destination selection, corruption retention, privacy/symlinks and unchanged
foreign-file bytes/SHA, copied-header wrong schemas/triggers, bounded lock
contention and FIFO refusal. It starts no daemon or network client. Tests were written
first in source; red execution was unavailable because the external author had
no admitted native build recipe. Compilation and behavioral execution belong to
CI plus independent review, not the standalone formatter.
