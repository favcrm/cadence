# Actual private runtime source (CAD-1159 / CAD-1160 / CAD-1161)

PR809 remains draft. This is source wiring, not an installed/qualified runtime. Immutable image roots remain unconfigured. No old artifact qualification, guest signer or operational readiness is inherited. The first bounded delivery is fresh ephemeral protected Store + one actual Pi task + streamed stdio/events + cancellation/owned physical retirement/discard/fresh reconstruction. Durable capture/restore is the second milestone, not a first-release waiver of startup provenance/currentness or a full-ticket completion claim.

## Actual construction and caller origin

After the separate signed runtime release/current, Root retains `Layout`, creates two new listeners, and constructs `OwnedDaemon` from the **held measured Client artifact**, empty argv/environment and cwd `/`. The fixed Client image has a private runtime entry; there is no caller-selected new executable/pin or PID adoption. Root owns fork/pidfd/birth/executable and held user/pid/mount namespace objects. An execveat seccomp/tracer stop must match the exact own-held FD/empty path, followed by the selected exec stop/seal corroboration. The runtime child gets the same zero-capability/securebits239/NNP seal, but a **separate** default-deny runtime filter permits pthread-only clone, AF_UNIX transport and filesystem operations within the four fixed RW enclaves. No process fork, another exec, credential/capability/namespace change or ptrace is allowed. Root detaches after the selected exec: subsequent TRACE exec without a tracer returns ENOSYS. Original enrollment child seal/filter is unchanged.

`OwnedDaemon::require_peer` samples the **actual** accepted UnixStream against the retained own-created daemon, birth/pidfd/executable/namespace and sealed credentials. Neither generic RuntimeProof, claimed PID/UID, JSON, a hash or matching reference can create this object. Every Pi provision/launch/control and Store startup/acquire/current/consume admission checks actual caller and Root/layout/endpoint custody before and after effects/owner awaits.

Root-created/listened sockets are `/run/cadence/private/{pi-launch,store-owner}.sock`, create-new only, metadata21000:21000/0600 in private21000:0700. Their listener credentials are Root0:0. Root transfers the actual listener FDs4/5 into the own-created sealed UID21000 daemon/proxy; that process forwards each **actual accepted UnixStream** by SCM_RIGHTS over its own-created private FD3 UnixDatagram. Packet framing is bounded65536, one actual socket FD only, MSG_CMSG_CLOEXEC, all unexpected/truncated descriptors closed. No proxy-supplied caller description is accepted. Root retains inode/socket/mount correspondence; loss/replacement refuses.

## Actual Pi and public helper delivery

Root admits the actual daemon before `OwnerProfile::issue`, authentic external Pi-purpose issuance, provision, `PendingLaunch::spawn`, privileged Arm, SAME accepted stream sealed Consume, burn/current/ACK and trace-held selected Node release. The actual Root-created helper has the fixed NSS `cadence-launch` supplementary group before Arm; the existing helper frontdoor is not weakened. Root corroborates this group and exact sealed guest/shared groups.

Provision/Arm forwards `signed: LaunchPermit::signed_operation()` — the ORIGINAL actual signed scope/authorization, never constructed from `describe()`. Helper elects PUBLIC trust independently from fixed RO root:root0644/nlink1 `/opt/protected/image-qualification.jws`, the SAME canonical native-image-qualification signature and immutable compiled image-root lookup as bootstrap. Public `helper_image_trust(now_ms)` returns an opaque crypto-only `HelperImageTrust`; its Pi ring/image digests/expiry do not constitute caller/helper custody. No response-elected ring, receipt/grant/runtime fallback, argv/environment/key FD, signer or public key file election exists. The fixed two-file Pi profile/policy contract remains `CONSTRUCTOR-PI-WIRE.md`.

After actual Node release, Root writes typed Started, then byte0x50 with EXACT3 SCM_RIGHTS stdin(write)/stdout(read)/stderr(read) FIFO data pipes. Shared stdio adopts only those files and retains Root control; it never spawns SUID locally/adopts a PID/signals a guessed PID. Root retains actual RunningLaunch and finite Status/Interrupt/Retire. Physical exited/cleanup is NOT external durable retirement/FINAL. Deadline/EOF/lost ACK remains UNKNOWN.

## Bounded first native task

The same authenticated runtime-owner reply may carry bounded finite `commands` (max8 queued):

- `{type:"task",task:<hex32>,alias,model:<provider/model>,prompt}` (ONE per actual ephemeral runtime, prompt1..32768 bytes).
- `{type:"cancel",task}`.
- `{type:"retire",task}`.

Root retains the exact task/alias/model and forbids replacement/replay. Actual daemon private `native_task` registers one fresh Pi/managed row and builds the REAL PiAdapter with guestUID21001, fixed `/workspace` and fixed application-data PM `/workspace/pm`. This is not a public operator RPC or forged peer/endpoint identity. Alias/model/prompt are data; actual Root caller admission + immutable alias/model policy + authentic one-use Pi operation still precede helper/Node effects. PiAdapter reads real Node state; exact configured model is mandatory before the real `run_turn`. No silent model fallback or automatic provider-request approval occurs.

Actual adapter events/requests/turn-start/result are serialized as NDJSON and streamed in ordered16KiB canonical unpadded-base64url parts over the own-created private channel. Root checks task + monotonic part and emits finite `task-event {task,part,bytes}` over the retained actual provider carrier; each exchange has normal fresh owner/current and10s/32frames/256KiB bounds. These are task DATA, never credential/custody/ready/retirement claims. Cancellation delegates to actual protected control. Retirement first proves physical exit through Root-owned helper/pidfd objects, then retires the retained daemon adapter/turn worker. A result/adapter EOF/close alone is not that proof. Host durable scope/discard obligations remain separately enforced.

## Reentrant actual physical readbacks (Host implementation required)

Root emits `runtime-daemon-ready {reference:<own-created hex32>}`. Host may retain a PRIVATE RpcTarget only from its actual qualified retained provider ExecProcess/owned runtime carrier, **never from this metadata/reference/PID or old ACK**.

While Root awaits any runtime-owner reply, the same bounded channel services:

`{version:1,type:"runtime-readback",operation,barrierNonce,sequence:<SAME pending owner exchange>,query}`

Queries are closed finite schemas:

- `{type:"current"}` -> `{reference,revision:1}` from live Root-owned daemon/ns/layout/socket custody.
- `{type:"opening"}` -> `{reference,revision:1,purpose,path}`; requires the ACTUAL already admitted startup UnixStream retained by Root, not cold endpoint syntax. Host combines it with the actual owner-elected full LineageInstallation/expiry, never configure-invented DBid/incarnation.
- `{type:"database-current",reference:<original consumed opening permit ref>}` -> `{version:1,reference:<original construction ref>,revision:1,facts:<EXACT original facts>,phase:"consumed"}` ONLY after actual consumed startup and independently WAL-aware READ_ONLY committed schema32/identity/open-latch readback on the live retained DB inode. No same blocked Store mutex, immutable=1 reader, recovery writer or expired opening renewal. Host MUST independently prove durable consumed status + captured full launch/world/localDO/ABA/lifetime before and after.
- `{type:"maintenance",purpose:"close"|"witness",attempt}` -> retained actual controller intent.
- `{type:"witness",attempt}` -> actual committed/root-validated witness (capture/attestation second milestone below).

Reply: `{version:1,type:"runtime-readback-result",operation,barrierNonce,sequence,data}`. Unknown/nested malformed query, wrong common tuple/sequence, expired/lost actual kernel/runtime/caller/mount source, excessive frame/byte budget refuses. The public result is observation DATA; the positive authority is the actual retained source/Host-owned capability, not deserialization. No generic exec/sign RPC.

Root provider finite `store-database-current {reference}` must call Host `currentConsumedDatabaseState`, not expiry-removed old `store-current` or /v1/current. Store private `database_current` requires exact original Binding/ref/deadline and explicit outcome database_current/phase consumed. Acquisition/consume/preconsume opening/maintenance remain strict<=300s; the original deadline is never changed. Old /v1/current remains closed-schema/closing-refusing.

## Real maintenance/capture source — second milestone limitations

Commands `{type:"close",binding}`, `{type:"witness",binding}`, `{type:"capture",attempt}` are selectors, NOT grants. Actual private daemon worker drains/joins actors and flushes before acquiring/burning real close, then calls actual `close_owned`; witness calls actual `witness_owned` and exports ONLY the typed committed return. Root validates it independently against selected DB schema/identity/closure/owner_witness/highwater while actual daemon/pidfd/ns/mount remain live. Actual source proof, authoritative witness attestation/private producer and Host immutable artifact validation/ownership still need paired qualification; no fake metadata may supply them.

Physical capture creates a new retained Root-owned fixed-attempt SQLite file, independently reads the live closed DB, performs bounded SQLite backup, converts clone to standalone DELETE journal, verifies schema32/identity/latch/witness/highwater/quick_check and absent sidecars, fsyncs and hashes actual bytes, keeps actual daemon quiesced-RUNNING and checks custody before/after. Transfer is finite `store-capture-begin {attempt,witness,size,sha256}`, ordered `store-capture-chunk {attempt,offset,bytes}`, `store-capture-commit {attempt,sha256}`. Ambiguous target is never deleted/reused. Root transfer/hash/exit/upload does NOT imply Host external validation, immutable storage, private artifact attestation, captured/restore-ready/FINAL or retirement. Genuine Host producer/capture persistence and restore qualification remain the second milestone, no full-ticket closure claim.
