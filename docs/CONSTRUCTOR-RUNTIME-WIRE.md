# Protected runtime handoff — CAD-1159, CAD-1160, CAD-1161

Source-WIP contract, same PR809. Not deployment, qualification or readiness.
Enrollment consumption does NOT authorize runtime lifetime, Pi or Store.

## Separate authority and retained carrier

The SAME provider-owned fixed root constructor carrier stays held by the
actual Native owner after consumed ACK and fresh consumed-current. Host retains
its exact global/company DO capabilities; neither guest snapshots nor caller
JSON reconstitute them. Loss/expiry is UNKNOWN; no inferred retirement/retry.

Host sends exact NDJSON:
`{version:1,type:"runtime-release",operation,barrierNonce,authorization}`.
Compact canonical Ed25519 authorization H.P.S, unpadded base64url. Signature:
`cadence.protected-runtime-owner.v1` + NUL + ASCII `H.P`.
Header: `{alg:"Ed25519",issuer:"agenticos-native-owner",kid,keyVersion,
 type:"protected-runtime-owner",version:1}`.
Payload: `{version:1,bindingJson,runtimeGeneration,issuedAtMs,expiresAtMs}`.
Binding is the exact own-created enrollment tuple; generation is actual enrolled
recipient generation, not a caller choice. Lifetime <=24h and qualified image
expiry. Replayed tuple/new constructor generation refuses.

Independent image qualification elects a DISTINCT optional `runtimeTrust` public
ring, same exact PublicTrust schema/bounds. Missing means runtime unavailable.
Neither receiptTrust nor grantTrust elects this purpose. Old image manifests
omit runtimeTrust and retain their canonical bytes. The signer exposes only a
finite `signProtectedRuntimeOwner` producer based on actual captured ownership;
no generic sign(bytes), guest keys, caller-selected root or ACK-derived issue.
This new source/artifact requires new root qualification; none is inherited.

## Finite private host operations

Each request carries version1, original operation/barrierNonce and positive
JS-safe monotonic sequence, plus its exact selected fields:
- `runtime-current`
- `runtime-daemon-ready`: actual Root-owned daemon reference, NOT serving
- `runtime-serving-ready`: same reference only after actual physical serving capture
- `task-event`: task32hex, zero-based ordered part, canonical base64url bytes1..16384
  Per-task output budget (AOS accepts at most 65,536 decoded bytes in 4,096
  consecutive parts, else it aborts the runtime): the last part and the marker's
  bytes are reserved, so data is limited to 4,095 parts and 65,536 minus the
  marker length in bytes. Output within that is sent unchanged. Beyond it the
  stream is cut at a UTF-8 boundary and ends with the marker
  `\n{"type":"output-truncated"}\n` (a whole NDJSON line); every later event is
  dropped silently and the stream stays Ok.
- `task-retired`: task32hex only after actual Root family exit and daemon worker/
  adapter/stream quiescence; observation, not provider/successor retirement authority
- `store-startup`: NO caller-selected binding/purpose/attempt; actual owner elects facts
- `store-acquire`: purpose init|restore|open|close|witness, attempt32hex
- `store-consume` / `store-current`: opaque reference
- `store-database-current`: original consumed opening reference; separate live DB
  authority, never an activation deadline renewal
- `pi-acquire`: typed OperationScope from pi_guest/owner.rs
- `pi-consume` / `pi-current`: reference32hex and same exact OperationScope

Response exact camelCase wrapper:
`{version:1,type:"runtime-owner",operation,barrierNonce,sequence,
 leaseExpiresAtMs,current,store,pi}`.
Current uses the existing full-binding/consumed/global/company/epoch/lineage/open
schema; fresh lease <=30s, never later than runtime authority expiry. Root checks
correlation, full binding, epoch/lineage/closure, wall-clock regression and actual
root custody. Store/Pi domain-specific code must validate exact issued facts,
durable phase, scoped revision/ABA and appropriate outcome. Runtime current alone
cannot issue a Pi or Store one-use grant. Only requested domain reply is nonnull.
Optional `commands` carries at most8 closed finite command objects; task/cancel/
retire are Host-to-Root DATA selectors, not new Root request types or permission.
Ready/task events/task retirement carry no Store/Pi reply. Physical readbacks
remain reentrant inside the SAME pending exchange; see CONSTRUCTOR-PRIVATE-RUNTIME.md.

Root requests no Store binding/path/witness claims. Actual fixed company lineage
methods acquire/consume/current return StorePermitFacts and durable issued versus
consumed phase. Consume burns before effects and lost ACK remains UNKNOWN.
Pi OperationScope/OperationReply exact schema lives in pi_guest/owner.rs; its
reference comes from the actual external one-use owner, not a local UUID/phase.

Each bounded exchange has <=10s,64KiB/frame INCLUDING delimiter LF,32 frames/256KiB bidirectionally.
Bootstrap THROUGH runtime-release stays ASCII. Only the separately verified
retained runtime operation selects strict UTF-8 JSON, with no normalization or
fallback. Complete serialized envelopes (including escaped text and LF) count,
not decoded prompt length. Raw CR/NUL, BOM-prefix, malformed/truncated UTF-8,
EOF without LF and excess budgets refuse; channel failure remains sticky.
Signed canonical payload validation and exact authority/correlation are unchanged.
Separately authenticated runtime scope permits a new operation window, preserving
pending bytes and fatal state. Enrollment's existing budget is not weakened/reset.

## Actual fixed topology and process creation

Root's private namespace and immutable graph remain RO. Only four fixed
self-bind enclaves clear RDONLY, retaining NOSUID/NODEV; root never remounts the
whole root RW: `/srv/cadence/protected/store`, `/srv/cadence/guest-views`,
`/workspace`, `/run/cadence/private`. Held inode and kernel fdinfo mnt_id plus
live path and flags are rechecked. Image owner preprovisions exact topology:
protected/store/private supervisor21000:primary21000 mode0700; views/workspace
supervisor21000:shared mode0750; trusted ancestors root:root0755. Store target
is ONLY `/srv/cadence/protected/store/cadence.db`. Graph is `/opt/cadence/pi`;
canonical RO `/opt/cadence/pi-profile.json` and `/opt/cadence/pi-policy.json`
are elected by signed piGraph/policy hashes, never caller hashes.

Root, NOT the sealed NNP/NOSUID supervisor, own-creates the fixed helper with
real21000/effective0, held execveat, empty environment, cwd slash and validated
routing selectors only. Opaque OwnedHelper holds actual Child/pidfd/birth and
selected helper inode. Exit-kill/exec tracer is retained before authorization.
`require_peer(stream,Privileged|Sealed,until)` checks kernel peer against THAT
actual child, executable, credentials/tracer and postseal all-zero five caps/NNP.
`release_node(permit,until)` corroborates actual selected Node exec stop/inode/
SHA/caps and fresh durable consumed operation before continuing. No caller PID,
fd/hash/UID/status JSON or generic Proof constructs an owned helper.

Fixed supervisor21000 proxy may accept a root-created listener and forward the
actual accepted UnixStream by SCM_RIGHTS over its finite private parent channel.
Root samples SO_PEERCRED itself against its own handles. Proxy data never mints
authority. Return ONLY fixed Pi stdin/stdout/stderr streams to the supervisor;
helper connects fixed endpoint AFTER close_fds and retains same channel through
postseal consume. No helper authority argv/env/arbitrary inherited descriptors.

## Current source integration and remaining qualification

Runtime signature/current/finite relay, fixed RW mount custody, private21000
proxy/dispatcher, actual own-created daemon registration, Root-owned helper,
stdio adoption and physical listener handoff are source implemented. Protected
daemon requests authentic no-argument startup and calls Store::open_owned before
SQL, retaining migration authorization and refusing Legacy fallback. Root admits
actual accepted sockets against retained kernel custody, not claimed PIDs/UIDs.
Serving requires retained listener/process/path objects and original consumed
DB/current; terminal stop cannot rearm. See CONSTRUCTOR-PRIVATE-RUNTIME.md for
same-runtime task generations and actual family plus worker-quiescence fences.

The finite enrollment ACK is still not runtime/Store/Pi/serving authority. Host
retains the SAME carrier only after separate purpose-authenticated runtime
release within the original bootstrap budget, then retains exact Company/Global
owner capabilities for bounded current/readbacks. Loss/expiry is UNKNOWN. A
successful release does not renew Store activation: business writes require the
distinct original consumed DB-current, not an expired permit or another acquire.
The first pilot permits only fresh Init with absent DB and all sidecars. Durable
close/witness/standalone capture source is partial second-milestone work, NOT
immutable artifact acceptance, restore-ready or FINAL.

These are source contracts, not observed native serving outcomes. Independently
owned bad-case checks must exercise actual production authority/current/peer
fences from a genuine baseline, not cold endpoints or shape-only checks. Required
coherent-head floor/CI, two noncontributor native reviews, exact-head operator
approval and Root-owned NEW image/kernel/provider/Pi/tool qualification remain.
No previous artifact qualifies changed constructor images; native stays disabled.
