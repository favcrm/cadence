# CAD-1159 / AOS-159 paired constructor wire v1

Status: **root construction/enrollment source wired; feature remains draft**.
Independent image-authority pins are unconfigured, so activation stays closed.
This is not kernel/provider qualification, deployment or operational evidence.
Protected Pi and external Store/restore are separate gates being integrated in
the same feature/PR; enrollment ACK cannot substitute for them.

## Fixed custody and artifacts

The platform retains the actual `Container.exec` process and provider-owned
stdin/stdout/stderr: `/opt/protected/bin/cadence-root-constructor`, no arguments,
UID0, cwd `/`, empty environment, no PTY. It retains exact global/company DO
object capabilities outside the guest. No guest HTTP credential, signing key,
generic RPC target, caller PID or observer diagnostic creates custody.

The constructor actually creates both children, private socketpairs (fixed
inherited transport FD3 only), stdio pipes, pidfds, held executable inodes and
namespace handles. Child identities are installer21000 and recipient21001.
The new fixed `/opt/protected/bin/cadence-enrolled-recipient` is the manifest's
`supervisor` artifact, **not** a generic daemon invocation. Previous five-bin
qualification does not qualify either new executable.

The private engine uses locked securebits239, empty capabilities, NNP1, fixed
`execveat` and default-deny x86_64 seccomp. A retained constructor tracer
continues exactly the initial held exec and corroborates its exec stop; later
exec attempts stop and refuse. Fork/clone, ptrace and namespace alteration are
not allowlisted. `PTRACE_O_EXITKILL` and owned pidfd cleanup bound physical
custody. Unsupported syscalls and unexpected states refuse.

The constructor requires exclusive root custody: other root/reserved-principal
or capability-bearing processes refuse. It creates its own private mount
namespace and recursively seals the tree read-only/nosuid/nodev, then measures
the finite selected executable topology. All retained namespace/image/process
facts are rechecked. An elected provider unable to supply this isolation or the
required mount/ptrace/pidfd/seal primitives stays refused. No verified boolean,
signature or hash substitutes for those syscalls. Writable protected Store/Pi
provisioning topology needs its own explicit bounded owner contract; this root
enrollment namespace does not silently grant mutable paths.

**Positive custody/enrollment/owner factories run only in the root operation
that owns the actual children.** Sealed UID21000/21001 children cannot inspect a
UID0 parent's executable through cross-UID procfs; no such access or forged
parent JSON is assumed. Their FD3 peer credentials authenticate transport only,
not executable custody. Children independently verify the signed format and
full binding, but cannot consume, enroll, sign, launch Pi or mint authority.

## Framing and bounds

One ASCII JSON object per newline; no EOF-as-message. At most65536 bytes per
frame including newline,32 frames and262144 bytes total across both directions.
Absolute operation deadline10s begins before provider exec, not receipt arrival.
Cleanup gets at most2s for the pair; timeout/kill/exit is NOT trusted retirement.
Error, disconnection, expiry or lost consume ACK is UNKNOWN with no retry and
external durable physical obligations retained. Child transport stays alive
through the platform's post-ACK current check, until carrier closure/deadline.

## Configuration and independent build trust

Initial host object, exact fields:

```
{version:1,type:"configure",operation,barrierNonce,expiresAtMs,
 launch:NativeLaunchBinding,imageAttestation:<compact>,
 lineage:{reference,databaseEpoch}}
```

Operation equals `launch.request.challenge`; manifest image equals
`launch.request.image`. `NativeLaunchBinding` is the platform's existing
`{request:{identity,purpose,challenge,image},epoch}`. The platform obtains
lineage from its authenticated external owner, not restored guest files.
Configure-supplied root keys, diagnostics, installer PIDs, extra fields and
nested duplicate fields refuse before construction.

`imageAttestation` is unpadded base64url canonical `H.P.S`, recursively sorted
JSON keys. Ed25519 signature input:
`cadence.native-image-qualification.v1` + NUL + ASCII `H.P`.

Exact header:

```
{alg:"Ed25519",issuer:"agenticos-native-image-owner",keyVersion,kid,
 type:"native-image-qualification",version:1}
```

Exact payload:

```
{version:1,image,source,
 artifacts:{constructor,client,carrier,observer,supervisor,helper,node,piGraph,policy},
 notBeforeMs,expiresAtMs,
 receiptTrust:[{issuer:"agenticos-native-owner",kid,keyVersion,publicKey}],
 grantTrust:[{issuer:"agenticos-native-owner",kid,keyVersion,publicKey}]}
```

Source is40 lowercase hex; artifact hashes64 lowercase hex; public keys are
raw32-byte base64url. Each purpose-specific trust ring has1–8 exact identities.
All numeric fields are unsigned JS-safe integers; keyVersion is positive int32.
Signature verification selects only independently compiled image-authority
public pins. The table is currently unconfigured; the manifest cannot append a
root. No private key crosses this channel. Signed public receipt/grant pins do
not substitute for independent verifier pins at the company owner. Root owns
public pin election, rotation/revocation and actual signer deployment.

## Finite operation

Broker `constructed` contains exact version1/type/operation/barrierNonce plus
actual held `installer` and enrolled `recipient` records. The platform persists
PREPARED before signing and sends
`{version:1,type:"release",operation,barrierNonce,receipt,grant}`. Root verifies
both envelopes and exact canonical binding against its own-created tuple.

Only phase-specific broker requests exist:
`{version:1,type:"prepared"|"consume"|"consumed-current",operation,barrierNonce,sequence}`.
Responses:
`{version:1,type:"owner",operation,barrierNonce,sequence,outcome,current}`;
outcome is `prepared`, `consumed` or `unknown`, current null only for UNKNOWN.
Exact current object:

```
{bindingJson,phase:"prepared"|"consumed",global:<64hex>,company:<64hex>,epoch,
 lineage:{reference,databaseEpoch},closure:"open"}
```

Hashes cover authenticated retained owner snapshots, excluding ONLY installer
history so the expected prepared-to-consumed phase change keeps the stamp.
Epoch, closure, lineage, full binding and owner ABA revisions remain. Hashes
alone are not authority; samples come from the retained private platform
operation capability and owned carrier. Sequence/kind/correlation are checked.
The consume attempt is burned BEFORE send; ACK loss cannot retry.

Root uses the existing `installer_enrolled` current-owner/consume core. It
checks PREPARED before releasing the child transport barrier, checks current
and held-process custody before/after each child handoff, consumes once, checks
fresh consumed-current, and then completes both finite child ACK handoffs with
fresh current rechecks. The expected owner stamp cannot change across awaits.
Only then it emits
`{version:1,type:"ack",operation,barrierNonce,recipientGeneration}`. The platform
also requires consumed-current after ACK. This is consumption-only evidence,
never protected Pi launch, serving readiness or physical terminal state.

## Completion boundary

The source positive root entry, private child transport, production custody,
recipient/public trust/phase-specific owner factories and release/consume path
are implemented. No acquired privileged execution, keys or qualified artifacts
were used to claim their operation. The independent source-authenticator bad
case remains owned by its author, unchanged. Remaining coherent feature work:
protected Pi owner launch/provisioning and Store lifecycle/open/restore gates,
paired private interfaces, required checks and final independent exact-head
Standards/Security reviews. Root-owned qualification and human exact-head
approval remain before activation or enqueue. No earlier artifact evidence is
transferred.
