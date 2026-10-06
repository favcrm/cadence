# CAD-1161 / AOS-160 exact Store lineage relay contract

Source alignment, not final review or operational qualification. Same Cadence
PR809 / platform PR349. Backend owns only installer-lineage new files; platform
coordinator owns current-owner/Company/Global/shared registration. Cadence
coordinator owns actual root constructor/caller registry/daemon/private relay;
Store contributor owns Store schema/seal/owner adapter. No guard edits.

Originally aligned at platform1c21ac01635d086849b6ebf3d266939f6a240b12;
current source reconciliation reads platform4f859e20d4e2031708020d9274ae98d89d8a7c59
installer-lineage-owner/service/construction.ts and actual CompanyControl/carrier
against Rust store/owner.rs, owner_transport.rs, schema.rs and constructor
lifecycle/dispatcher/layout. Historical prepared-only/ACK cleanup and missing
server descriptions are not current diagnostics. No operational qualification
or new request/response schema is inferred from this source alignment.

## Four different references — do not substitute

1. Full NativeLaunchBinding {request:{identity,purpose,challenge,image},epoch}:
   preserve ALL identity members, optional imageLane included, role/purpose,
   image/generation and epoch. operation is EXACT request.challenge UUID.
2. LineageInstallation: {launch,databaseId,incarnation,reference,databaseEpoch,
   expiresAtMs}, all owner-elected/persisted, NEVER read from a guest snapshot.
   databaseId is stable logical DB identity; incarnation fresh on init/restore;
   reference is the NEW permanent lineage record; databaseEpoch monotonic >0,
   initial1, restore strictly newer than source. All numbers JS-safe unsigned.
   Tokens backend accepts [A-Za-z0-9_-]{1,128}; this stricter set is accepted by
   Rust (which otherwise allows bounded identifiers). No literal IDs are elected
   by constructor, PID/hash/path/claimed bool or caller body.
3. StorePermitFacts.reference is a DISTINCT one-use permit correlation, not a
   bearer credential or lineage reference. Exact facts:
   {reference,lineageReference,launch,binding}. lineageReference MUST equal the
   selected installation.reference (and captured constructor lineage reference),
   binding DB identity/epoch equal installation, launch exact captured full tuple.
4. StoreOpeningObservation.reference/revision identifies the retained actual
   construction/custody readback capability, NOT either reference above and NOT
   a serialized capability. The authentic CompanyControl-produced RpcTarget
   observeOpening() must retain actual new runtime/provider/root operation and
   actual owned daemon/accepted kernel peer/mount/inode custody. Matching JSON
   reference/revision or RuntimeProof alone cannot create it.

## Existing /v1/current — unchanged closed schema

SERVICE POST body {version:1,launch:<FULL actual NativeLaunchBinding>}.
Response EXACT {version:1,operation,epoch,image,reference,databaseEpoch,
expiresAtMs}, matching launch.operation/epoch/image, reference owner-elected,
DB epoch >0 JS-safe, future expiry <=5min. Constructor configure continues only
lineage:{reference,databaseEpoch}; it has NO databaseId/incarnation.
Do not add DB fields to /v1/current silently: current consumer denies extras.
Full DB identity comes from separate authentic StorePermitFacts/installation
readback after NEW runtime release and actual protected caller admission.
No conversion of old enrollment ACK to this authority.

## Exact Rust StorePermitBinding (snake_case)

{database_id,incarnation,database_epoch,operation,purpose,path,challenge,attempt,
artifact,deadline_unix,source}. Unknown fields/duplicates refuse.
- purpose init|restore|open|close|witness, no arbitrary verb.
- path ONLY /srv/cadence/protected/store/cadence.db (not cadence.sqlite3).
- challenge ARRAY of32 byte integers0..255, not hex string, nonzero.
- attempt owner-issued32lowerhex recommended (existing backend elects this).
- artifact owner-selected bounded identity, not client filename/digest authority.
- deadline_unix positive JS-safe INTEGER SECONDS, expires>now, <=now+300sec;
  opening deadline <= installation/runtime/image authority expiry. Never ms.
- source NULL for init/open/close/witness. For restore ONLY:
  {incarnation:<source different from target>,database_epoch:<positive strictly
   less than target>,sha256:<nonzero64lowerhex SHA of externally validated exact
   standalone SQLite artifact>}. Same logical database_id, fresh target incarnation,
  artifact bound to actual source capture/witness. No tombstone grants restore.

## Actual durable backend methods and phase relay

Use existing authentic methods in installer-lineage-service.ts:
acquireStoreStartup(company,operation)->StorePermitFacts|null; it obtains real
nativeLineageStoreConstruction RpcTarget and twice-fenced observeOpening,
then elects facts under revision transaction. Guest startup is no-arg.
Purpose-scoped acquireStoreInit/Restore/Open/Close/Witness(company,operation,
attempt) accepts selectors ONLY; producer is authentic namespace owner, not
caller Binding. All facts must be exact independently current owner facts.
currentStorePermitState(company,reference)->{facts,phase:"issued"|"consumed"}|null.
consumeStorePermit(company,reference)->boolean durably CAS issued->consumed.
Never fabricate phase from locally attempted consume or a echoed client flag.

Host/root runtime carrier finite requests retain original operation/barrierNonce/
sequence and separately authenticated runtime lifetime:
store-startup with NO caller fields; store-acquire {purpose,attempt};
store-consume/current/database-current {reference}. Startup owner elects the
purpose/challenge/attempt, not a new guest-selected store-init-acquire frame.
Root-host runtime-owner reply must have store:<EXACT {facts,phase}>,pi:null,
plus genuine existing full-binding consumed OwnerCurrent and <=30s lease.
For acquire, obtain currentStorePermitState after durable issue and demand issued;
for successful consume obtain genuine consumed current before returning success.
Failure/null/false/mismatch/expired/lost ACK is UNKNOWN, no retry/reissue. Exact
facts stay frozen across requests. An outcome label alone cannot grant authority.
The actual Root StoreSession now strictly deserializes {facts,phase}, matches
original full NativeLaunchBinding/lineage/DB tuple and expected durable phase,
and admits the retained own-created daemon/kernel stream before/after replies.
The private capability is produced only over that fixed authenticated channel,
not reconstructed from Value or a generic RuntimeProof. Live business writes
use distinct original consumed database-current, preserving the opening deadline;
this is not activation renewal. Source wiring is not native runtime evidence.

## Fixed guest-private service and returned grant

/run/cadence/private/store-owner.sock, supervisor21000 directory0700/socket0600,
actual ROOT-created/listened server SO_PEERCRED UID0:GID0. Transport credential
only; server admits actual root-owned daemon through retained kernel registry,
never UID/PID/JSON alone. Optional sealed proxy forwards actual accepted FD.
BE-u32 JSON max32KiB,10s fresh IO, one retained stream, sequences starting1.
Startup request EXACT {type:"startup",version:1,sequence:1}, NO caller binding,
purpose/path/incarnation/hash. Root host owner selects init|restore|open facts.
Acquire uses {type:"acquire",version:1,sequence:1,binding}; root matches selector
against actually issued authority, cannot elect from it. Consume/current carry
sequence/grant/binding EXACT original issued tuple on SAME retained channel.
Response EXACT {version:1,sequence,grant:<facts.reference>,binding:<facts.binding>,
outcome:"issued"|"consumed"|"current"|"init_file"|"database_current"|"unknown",
phase:"issued"|"consumed"}. The external facts.reference is the original canonical
UUIDv4, distinct from Root's hex32 construction reference; never normalize or
replace it. Backend facts.launch+lineageReference are Root-validated, not dropped unchecked.

Init-only file delivery request EXACT
{type:"init_file",version:1,sequence,grant,binding}, original consumed tuple.
After its exact init_file/consumed JSON reply, Root sends ONE byte0x44 with ONE
SCM_RIGHTS regular O_RDWR File; receiver requires CLOEXEC, no extra/truncated
controls, actual FD/name/private-DAC/empty-file correspondence. JSON and FD share
ONE absolute10s request budget. Delivery burns independently BEFORE wire, never
resets consume or retries after loss. Other JSON frames require zero descriptors.

## Consumption ordering / init / restore

StoreOwnerGrant is nonClone/nonDeserialize private capability, not Binding.
Store::open_owned(grant) issues private StoreOpenPermit for init/restore/open only.
Actual schema.rs ordering: fixed path/mount-custody check, authentic current,
consume burn BEFORE first SQLite connection/file creation, consumed-current,
then parameterless private permit.init_file BEFORE first memory SQLite connection,
then memory/target work with ALL repeated authentic current checks. Client local burn occurs
before wire send; external CAS must occur before guest mutation. Any loss leaves
permanent consumed/UNKNOWN obligation; no fresh retry/erase/fallback.

INIT: authentic initial epoch1/no restoredFrom/sourceNULL. Main DB and all three
sidecars must be absent before AND after issue. After local burn and actual
external consumed/current, Root itself create_new0600/fchown21000:21000 and
retains the actual creation File/held mount/own-daemon stream/original facts
BEFORE Consume ACK. Loss leaves a spent partial file, never deletion/retry.
Root opening current now corroborates that SAME held creation through incomplete
construction (including private SQLite sidecars); it neither re-elects absence
nor prematurely claims completed identity/latch. Store receives that original
File once before SQLite, builds reviewed schema32/latch/incarnation in memory,
seeds durable closed-refusal destination before backup and retains/syncs the File.
No arbitrary empty/existing file is admitted. ALL original activation deadlines,
peer/current/full-facts checks remain; final distinct database_current alone
verifies committed WAL/schema32/identity/open-latch. No Legacy fallback.

RESTORE: backend permanent source must actually be captured/witnessed and CASed
to restored for this exact target; newer DB epoch/fresh incarnation/new lineage
reference but same DBid and full elected target launch. Externally validated
standalone file digest/absence of WAL/SHM/journal; read-only immutable preflight
requires reviewed schema32, exact source incarnation/epoch, closed+done latch,
unique matching owner witness and business-highwater capture. Only then consumed
permit allows RW, under BEGIN IMMEDIATE revalidate/rebind to target identity and
clear closure latch. Restored file/markers/closure don't elect authority. No
source->target artifact correction, snapshot adoption or replay.

OPEN: exact current target DB identity/operation/latch open; existing initialized
provenance and authentic held custody. A consumed init/restore permit is NOT proof
initialization completed or WAL succeeded. Backend existing startup election can
use consumed initial permit only as replay prevention/precondition; actual owned
opening observation must independently corroborate completed held identity.

## Closure/capture/retirement remain separate

close/witness new purpose permits with exact DB identity/challenge/attempt/
artifact/deadline; all-writer barrier/writer-lock maintained. Witness returns
sequence distinct from businessHighwater. Actual QuiescedWitness facts are NOT
uploaded/validated capture or FINAL. Root capture owner must authenticate actual
quiesced running instance/artifact hash and permanent readback; only then backend
captures/restores. Physical stop/exit/timeout/EOF, consume ACK or backup filename
never retires durable obligations. Captures and revoked ancestors remain permanent;
failures/replay/current ABA/epoch/closure/revocation refuse without reissue.

## Current construction source and remaining native qualification

Platform StoreOpeningObservation shape remains EXACT
{version:1,reference,revision,installation,purpose,path,deadlineMs}. Actual
CompanyControl obtains its private RpcTarget from the retained Native runtime
carrier; Root supplies finite opening readback only after authentic runtime
release/current, retained own-created daemon and held fixed mount/path custody.
Matching JSON/PID/UID/RuntimeProof alone cannot create this reference or grant.
The actual parent kernel registry, private Store server and no-argument daemon
startup are source integrated, not proof that a new native image has run.

Root recursively seals first, then after authenticated runtime current self-binds
ONLY four preprovisioned fixed data leaves RW, retaining NOSUID/NODEV and held
FD/dev/inode/kernel mnt_id/DAC correspondence, BEFORE sockets/daemon/startup SQL.
Pre-seal RW provisioning alone is insufficient. Store path is cadence.db, never
cadence.sqlite3 or a caller profile; immutable executable/Pi graph remains RO.
The first pilot allows only fresh Init and verifies genuine NotFound for DB and
all SQLite sidecars before/after owner startup; Open/Restore stay inactive second
milestone paths. Host pre-enrollment installation is a durable obligation, not
proof of physical freshness.

Enrollment ACK grants no lifetime/serving. Separately purpose-signed runtime
release completes under the ORIGINAL bootstrap budget, then the SAME carrier
and exact Company/Global capabilities are retained for bounded domain current.
Loss/expiry remains UNKNOWN, no activation renewal or inferred retirement.
See CONSTRUCTOR-RUNTIME-WIRE.md and CONSTRUCTOR-PRIVATE-RUNTIME.md for current
lifetime and physical serving/task contracts. Root NEW image/kernel/provider/Pi
qualification, genuine authority/current bad-case baseline, required coherent
floor/CI, two noncontributor reviews and exact-head operator approval remain.
Capture source is NOT immutable artifact acceptance or FINAL. Independent guard
bodies unchanged; no broad fixtures, operational keys or readiness claim.
