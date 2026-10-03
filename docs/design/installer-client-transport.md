# CAD-1113: fixed private installer client transport (source only)

## Design note

A private Linux client transports one bounded host-stdin capsule to the fixed
`/run/cadence-supervisor/grant.sock`. There is no generic CLI, RPC, HTTP route,
selectable path/command, environment secret, temporary capsule file, signer or
launcher. The fixed bundle has three no-argument executable entries; all remain
closed at independently qualified image/bootstrap factories.
The production entry resolves unavailable admission/trust/consume factories
**before reading stdin**. No source in this batch installs or runs the client.

The concrete socket mechanics are nonblocking connect, bounded send/receive,
`poll` with a single absolute monotonic deadline (10 seconds), and exact response
classification. There is one connection and one request; no retry exists.
Malformed, missing, oversized, truncated, late, or uncorrelated acknowledgements
are UNKNOWN. A consumed acknowledgement is evidence of a transport reply only,
not protected-launch success, eligibility, release or physical retirement.

## Private correlated wire profile

The existing grant signature/claim schema is unchanged. Legacy `challenge` and
`install` keep their existing admission policy. A versioned transport profile
adds operation and **two separate** generations (installer and recipient):

```text
challenge-v1 <operation> <installerGeneration> <recipientGeneration>\n
install-v1 <operation> <installerGeneration> <recipientGeneration> <grant>\n
```

The operation is a lowercase UUID; generations are 32 lowercase hex bytes.
There is exactly one space between fields and one newline. No normalization,
extra fields or negotiated verbs. Host stdin additionally appends a compact
installer receipt to the install frame, before its newline; the client verifies
that receipt with the existing receipt consumer against production pinned trust
and requires its canonical binding bytes to equal the externally enrolled
prepared binding. The receipt is not a second grant or an enrollment mutation.
Neither secret envelope is echoed or included in diagnostic errors.

All separators, receipt bytes and the terminating newline count toward the
32 KiB stdin cap. The framed socket request is independently capped at 32 KiB.

Only after the existing kernel/pins/signature/consume guards does the server
produce the corresponding response:

```text
ok challenge-v1 <operation> <installerGeneration> <recipientGeneration> <pid> <starttime>\n
ok consumed-v1 <operation> <installerGeneration> <recipientGeneration>\n
```

The connection closes after the single response. Exact byte equality binds the
operation and both generations; challenge PID/starttime must also equal the
separately enrolled and measured supervisor. EOF is part of the response frame:
trailing bytes/frames or an acknowledgement without completed EOF refuse under
the same deadline. Old, unbound replies are not accepted by the new client.
Before install consumption, the **existing** grant parser checks the request's
operation and recipient generation against the grant. No crypto codec or
second enrollment ledger is introduced.

## Held topology and process custody

The client retains no-follow descriptors for every node: `/` (root:root 0755),
`run` (root:root 0755), `cadence-supervisor` (21000:21000 0700), and the socket
(21000:21000 0600). Exact owner/group/type/mode policy refuses writable ancestors,
symlinks and unexpected topology. Connect addresses the held parent through
`/proc/self/fd`, not a second unanchored `/run` traversal. Re-open/re-stat checks
compare device/inode/owner/group/mode/ctime before sending and after receiving;
parent/leaf replacement refuses. Socket identity is not authority by itself.

The expected supervisor identity is privately enrolled, not extracted from the
peer response. Actual `SO_PEERCRED` UID/GID/PID, live `/proc` starttime, generation
binding, and bounded held executable digest must match it. Self-admission checks
real/effective/saved UID and GID 21000, no supplementary groups, all five actual
kernel capability sets empty, exact enrolled PID/starttime/generation/executable.
The executable measurement reuses the existing root-owned, regular,
non-writable, linked, bounded, stability-checked grant-consumer measurement.
Root UID alone and caller reports cannot produce an admission.

## Explicit unavailable owner boundary

The private `installer_bundle` now contains finite **source mechanics** for a
non-setuid carrier/drop/fdexec, bounded procfs diagnostics and waiting bootstrap.
It reuses existing capability sealing and held-FD hash/topology helpers. This is
**not** a privileged execution/observer qualification, durable current-owner
adapter, retirement adapter, operational trust manifest, installation, image
attestation, deployment or native eligibility result.
`production_admission`, receipt trust, durable consume, self-enrollment and live
listener factories remain unavailable. Receipt verification is format evidence,
not custody or current authority. Test paths and synthetic enrollment literals
exist only inside cfg-test mechanics and cannot open any production factory.

In particular, the existing PR688 receiver still requires a UID0 image-pinned
installer; the accepted future carrier/client proposal drops to UID21000. This
batch does **not** reconcile that policy mismatch or qualify a UID21000 client
for the UID0 receiver. A later explicitly reviewed owner must supply real
root/drop/procfs/enrollment/retirement/trust and compatible mutual admission;
caller metadata, synthetic digests or root UID cannot substitute. No ptrace
capability, key, signing principal, live listener or launch authority is added.

### Unelected fixed artifact and observable-policy selections

Proposed source literals are `/opt/protected/bin/cadence-grant-install`,
`/opt/protected/bin/cadence-installer-client` and
`/opt/protected/bin/cadence-installer-observer`, root:root0755 under held
root:root0755 ancestors. No artifact hashes or operational owner are elected.
New fdexec policy is Linux x86_64 little-endian ET_EXEC only, bounded128MiB,
with no PT_INTERP/PT_DYNAMIC/scripts/file capabilities/setuid/setgid. An
unqualified dynamic startup graph is refused, not guessed.

Construction-only bootstrap may permit one bounded withheld host stdin read;
it is separate from signed enrollment/release/current-owner authority. EOF,
trailing/oversized frame or deadline cannot open a socket or consume. Root
approved the concrete r3 checkpoint and distinct generation mapping. The new
private `installer_enrolled` route carries BOTH existing signed envelopes:
`enrolled-install-r3 <grant> <receipt>\n`, total32768bytes including framing,
one request plus EOF. Existing UID0 admission and the old independent
production-refusal acceptance remain unchanged.

`production_release_authority()` and `production_enrolled_receiver()` obtain
separate unavailable `production_closed_custody()` before caller proof or
effects. Protected recipient enrollment, image trust and authenticated
ExecutorAdmission+CompanyControl owner port also remain unavailable.
`release_host_frame()` is the actual private guardable release entry, without
test argv. The fixed waiting entry preserves its original absolute deadline
into this path; it never rewrites the frame to grant-only delivery.

The existing grant parser verifies its original domain/keyring, then parses the
verified receipt's canonical binding with the EXISTING challenge parser and
compares the full typed challenge, including imageLane presence. No new codec,
installerGeneration claim or signature schema is introduced. Private PREPARED
and CONSUMED-current observations compare exact canonical full binding bytes,
phase, owner revisions, current epoch, open closure and lineage. Unknown/lost
consume ACK is terminal UNKNOWN, never retried or locally reopened. Peer
credentials and held executable/procfs handles recheck before and after owner
calls/consume; no local replay ledger is authority.

Only after combined consume/current recheck may the receiver write
`ok enrolled-consumed-r3 <operation> <signedAttemptGeneration> <recipientGeneration> <barrierNonce>\n`
plus EOF. Client requires exact bytes and re-observes kernel self/recipient,
topology and authenticated consumed-current owner. Success represents only
consumption acknowledgement; no spawn, launch, enrollment or retirement call
exists. Test-only thread-local counters permit an independently authored guard
to check zero effects at the real missing-qualification paths; they never
supply a capability/override/factory in a release build.

Observer holds the real procfs PID directory and executable, compares actual
IDs (including filesystem IDs), groups, five caps, NoNewPrivs, single-thread
PID/TGID, starttime and user/PID/mount namespace correspondence before/after
measurement. Exact protected-path inode plus bounded offset-preserving digest
must match; a same-digest copy is insufficient. Target securebits/keepcaps are
not exposed by procfs: diagnostic fields stay unavailable/null and qualified
observation refuses. Querying the observer's own prctl would NOT measure the
target. Only the waiting client can inspect its own prctl for construction;
that is not external enrollment or earlier-root provenance. No ptrace fallback.
Root selected separate private `ClosedCarrierCustody`, binding independently
qualified immutable seal/no-alternate-exec custody to the exact construction
PID/starttime/client/carrier/observer and namespaces. Its private fields and
unavailable factory cannot be populated from diagnostics, observer-own prctl,
stdout, a signed receipt, env or caller assertions. The procfs diagnostic fields
remain null/UNKNOWN even when this separate evidence is checked.

## Acceptance and delivery

Ordinary-UID named Unix-socket checks exercise exact framing/correlation,
fragmented replies/stdin, finite deadlines, backpressure, malformed/truncated/
oversized/lost replies, held-path replacement/symlink/owner/mode refusal, and real
kernel admission refusal. They do not claim root custody or launch success.
The reviewer/ticket-author's independent bad-case acceptance check is mandatory
before review; implementer-written tests do not replace it. Required CI, two
independent exact-head reviews and the exact human audit decision precede merge.
PR686 retains heavy-validation priority. No operational change is authorized.
