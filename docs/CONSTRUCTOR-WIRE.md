# CAD-1159 / AOS-159 paired constructor wire v1

Status: **incomplete draft, activation closed**. This is the agreed source
contract, not elected artifacts, kernel/provider qualification or deployment.
The existing protected Pi and external Store/restore gates are independent.

## Fixed custody and artifacts

The platform retains the actual `Container.exec` process and provider-owned
stdin/stdout/stderr: `/opt/protected/bin/cadence-root-constructor`, no arguments,
UID0, cwd `/`, empty environment, no PTY. It retains exact global/company DO
object capabilities outside the guest. No guest HTTP credential, signing key,
generic RPC target, caller PID or observer diagnostic creates custody.

The measured new constructor owns both children, private socketpairs (fixed
inherited owner FD3 only), stdio pipes, pidfds, held executable inodes and
namespace handles. Child identities are installer21000 and recipient21001.
The new fixed `/opt/protected/bin/cadence-enrolled-recipient` is the manifest's
`supervisor` artifact, **not** a generic daemon invocation. Its implementation
and factory integration are still pending in this draft. Previous five-bin
qualification does not qualify either new executable.

The private child engine uses locked securebits239, empty capabilities, NNP1,
fixed `execveat` and default-deny x86_64 seccomp. A retained constructor tracer
continues exactly the initial held exec and corroborates the exec stop; later
exec attempts stop and refuse. Fork/clone, ptrace and namespace alteration are
not allowlisted. `PTRACE_O_EXITKILL` and owned pidfd cleanup bound physical
custody. Unsupported syscalls and unexpected states refuse. Immutable mounts,
exclusive trusted root principals and full artifact/namespace custody still
must be integrated before any activation: hashes and procfs cannot replace
those guarantees.

## Framing and bounds

One ASCII JSON object per newline; no EOF-as-message. At most 65536 bytes per
frame including its newline, 32 frames and 262144 bytes total across both
directions. Absolute operation deadline10s begins before provider exec, not at
receipt arrival. Cleanup gets at most2s for the pair; timeout/kill/exit is NOT
trusted owner retirement. Error, disconnection, expiry or lost consume ACK is
UNKNOWN with no retry and durable physical obligations retained.

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
Configure-supplied root keys, diagnostics, installer PIDs and extra fields
refuse. Nested duplicate fields refuse.

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
root. No private key crosses this channel. The signed public receipt and grant
pins do not substitute for independent verifier pins at the company owner.
Root owns public pin election, rotation/revocation and actual signer deployment.

## Finite operation

Broker `constructed` contains exact version1/type/operation/barrierNonce plus
actual held `installer` and enrolled `recipient` records from existing binding
schemas. The platform persists PREPARED before signing receipt/grant and sends
`{version:1,type:"release",operation,barrierNonce,receipt,grant}`.

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

The hashes cover authenticated retained owner snapshots, excluding ONLY
installer history so the expected prepared-to-consumed phase change keeps the
stamp. Epoch, closure, lineage, full binding and owner ABA revisions remain.
Hashes alone are not authority; their source is the retained private platform
operation capability and owned carrier. Every await must recheck current state
and expiry. Consume is once-only. Final
`{version:1,type:"ack",operation,barrierNonce,recipientGeneration}` is
consumption-only evidence, never protected Pi launch or serving readiness.

## Draft completion boundary

Current source supplies the real manifest guard, bounded framing and private
owned child engine. The executable reuses that guard but remains explicitly
closed: no constructed/PREPARED/ACK emitted. No production factory is claimed
complete. Remaining work stays in the same PR: immutable topology/root-principal
custody; fixed recipient; positive inherited context/factories; phase-specific
owner relay and full release/consume integration; paired-wire checks. Required
final independent Standards and Security reviews and human exact-head approval
remain before enqueue. No acquired privileged execution was performed.
