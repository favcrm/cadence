# CAD-1160 source API agreement — same CAD-1159 PR809

## Ownership — proceed, no new design permission round

Pi contributor owns pi_guest/**, bounded pi.rs caller, protected_pi_profile.rs
and cadence-agent-exec/protected.rs. Explicit owned-path commits only; no pushes,
shared edits or independent guard edits. Coordinator owns constructor/root
server, private listener/proxy, actual kernel supervisor/helper registry, daemon
bootstrap/registration, Cargo/lib/shared wiring and pushes. Existing Pi-owned
PendingLaunch::spawn/arm/serve is delegated service logic; coordinator invokes it
using actual opaque permit and own-created handles. No second duplicate generic
RootPiPermit/authority RPC is necessary. Native configured/effective/reported
model remains openai-codex/gpt-6.1-sol HIGH.

## Frozen selectors, service and transport

Fixed /run/cadence/private/pi-launch.sock. Existing exact Selection and
Provision/Arm/Consume Request/Authorized/Consumed/Refused fields in authority.rs
stand. No PID/key/path/pin/custody flag in requests. Provision is only an actual
root-registered owned supervisor; helper cannot call it. Arm is only the actual
root-created selected privileged helper. Consume is only that SAME accepted
connection after actual seal, exact reference/scope and authentic one-use
current. Wrong child, guest21001, same-UID impostor, changed generation/model/role
and replacement/ABA refuse before effects. Authoritative proof is retained
pidfd/birth/executable/namespace plus real accepted socket and current operation,
not SO_PEERCRED/PID alone, Caller JSON, RuntimeProof alone or a description.

Absolute30s, BE-u32 framing. Requests max4096 bytes; full elected profile response
max8MiB. No retry/reconnect. Helper connects AFTER close_fds, retains same socket
through seal and closes before selected exec. No authority argv/env/inheritedFD.
Root constructs helper; sealed NNP/NOSUID supervisor never elevates via local
setuid Command. Own-created helper Node exec remains trace-held until final
actual consumed-current/image/caps checks. Parent retains RunningLaunch; dropping
or transport EOF is physical cancellation UNKNOWN, not external retirement.

Listener credential correction: coordinator ROOT creates/listens on this
socket. Kernel SO_PEERCRED reports ROOT0 creator/listen credentials even if a
sealed21000 proxy later accepts/forwards it. Directory21000:0700/socket21000:0600
remain fixed, but client must expect serverUID0 for this topology, not21000.
This credential authenticates TRANSPORT only; it does not elect image/Pi purpose.
Parent validates actual owned caller on the REAL accepted UnixStream. An optional
sealed proxy forwards that actual FD by SCM_RIGHTS, never claimed credentials.
Original enrollment child filter/seals are NOT relaxed to turn children into
privileged helpers or arbitrary servers.

Bounded stdio adoption: existing contributor Launch/Started then byte0x50 with
EXACTLY3 SCM_RIGHTS stdin(write),stdout(read),stderr(read) CLOEXEC FIFO descriptors.
No executable/authority descriptors, no local PID adoption or guessed PID signal.
Parent owns handles and delegates finite Interrupt/Retire/Status on retained
supervisor control connection after actual caller admission and reference match.
Started.pid is observation only. Retire must not assert external durable retirement;
physical Exited is reported only from actual owned kernel wait/pidfd outcome.
Those call sites remain integration work, not claimed serving by this agreement.

## Elected profile/pins and NEW Pi-purpose signature

Fixed RO root:root0644 canonical recursively sorted JSON/no newline:
/opt/cadence/pi-profile.json contains exact ImageProfile I; manifest artifacts.piGraph
is its SHA256. /opt/cadence/pi-policy.json contains {version:1,routes:[alias,role,
models]}; artifacts.policy is its SHA256. Separate elected node/helper SHA must
match I. Root graph /opt/cadence/pi full sorted inventory/approved extensions,
no discovery/symlink/extras/submount. Node /opt/cadence/pi/node. Fixed helper
/opt/protected/bin/cadence-agent-exec (root-held manifest helper artifact), NOT a
second /opt/cadence/libexec helper election/localSUID path; contributor must align
its fixed helper validation to this actual held artifact. Mixed immutable graph
and explicit RW data mounts/finite provisioner remain as already agreed.

Accept proposed exact new Pi domain cadence.protected-pi-launch.v1 + NUL + ASCII
H.P. Canonical compact Ed25519 header {alg:Ed25519,issuer:agenticos-native-owner,
kid,keyVersion,type:protected-pi-launch,version:1}. Payload {version:1,reference,
scope:EXACT OperationScope,issuedAtMs,expiresAtMs}; JS-safe timestamps, <=30s,
no future issue/expired/rollback, expiry <= authenticated runtime/image expiry.
Keep SAME authorization in issued/current/consumed Pi replies. No generic sign(bytes)
or caller key. Root platform owns actual purpose-confined producer/signing service.
Signature alone never supplies live custody/currentness/one-use authorization.

Coordinator adds optional signed manifest piTrust PublicTrust ring1..8 with NO
receipt/grant/runtime fallback. Missing=>Pi unavailable; absent field omitted for
old canonical manifests. Public interface implemented in coordinator source:
PiKeyRecord=(String,String,u64,[u8;32]); pi_public_keys(until)->Result<&static[PiKeyRecord]>
and pi_expires_at_ms(until)->Result<u64>. Both obtain ONLY actual qualified root
context plus separately authenticated runtime lifetime and fresh root recheck.
No key/JSON/filename/env input, guest private key, test Root/Bootstrap mint.
Pi contributor owns authenticate_operation pure trusted-ring signature comparison
and opaque private result; real OwnerProfile gets ring/expiry only through this
interface BEFORE operation permit/provision/Node effects. The trusted helper
must receive independently authenticated public image/Pi purpose information,
not accept a ring echoed in Authorized or a guessed UID as its root of trust.
That helper trust delivery is bounded source integration, not solved by this note.

Full operation wrapper current scope equality is ALREADY coordinator source:
Pi exchange corroborates binding/global/company/epoch/lineage/database_epoch before
returning payload; external owner-issued reference, phase/current and consume burn
remain required. Root backend must not mint facts from guest scope JSON alone.

## What is implemented versus pending

Root runtime proof/current/finite Pi relay, Pi public ring/expiry getter, actual
OwnedHelper and Pi-owned PendingLaunch guard/stdio client are source present.
Shared StdioAdapter::adopt_protected now consumes opaque RemoteLaunch into ONLY
File stdin/stdout/stderr and retained RemoteControl, delegates interrupt/retire/
status_until, never spawns/adopts/signals the observed PID. Failed status/retire
stays UNKNOWN and cannot become no-child/Exited evidence. Legacy local paths are
unchanged apart from the shared typed stdin wrapper.
Actual owned supervisor/daemon registration, root private dispatcher/proxy, signed
Pi producer and complete authenticated helper profile delivery are still pending.
Proceed implementing owned source now; do not label proposed caller factories,
services or qualification as complete. Do not request another routine permission.

Focused unchanged original constructor guard PASS1/0 after this source extension;
required pre-push fmt/doctor PASS but clippy FAIL on unwired APIs, no suppression,
no later safety floor or final gates claim. No push while mandatory gate fails.
Independent guard remains untouched; its author extends SAME ONE check against
actual production guard, not cold endpoint/shape/PID validation or fake trusted
Root/keys. Required green gates and exact-head different independent native
Standards/Security axes/human approval precede enqueue. No earlier qualification
is inherited, no root keys/acquired execution/deployment or activation this turn.
