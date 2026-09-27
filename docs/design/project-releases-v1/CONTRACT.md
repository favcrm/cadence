# Project release ledger v1

Status: **proposed domain/contract increment for CAD-662**. The glossary, schema,
synthetic fixture and validation examples exist; release commands, board screens,
ledger persistence, authorization gates and import/deployment adapters do not.
No acceptance checkbox is satisfied solely by these examples.

## Existing owners and boundaries

`src/issue/work.rs` owns milestones in `PROJECT.md`: explicit outcome/schedule
metadata plus derived issue progress/health. `src/issue/model.rs` owns issue
frontmatter and generic PR/commit/note refs; it has no release fields. Its writers
can drop unknown fields. `src/issue/write.rs` already owns the shared CLI/HTTP
writer, tracker lock, revision conflicts, atomic file replacement and scoped git
commits. `src/rollout.rs` owns host lease/schema/backup/build evidence. None is a
generic publication ledger. Reuse these boundaries; never translate milestone
achievement, issue done, PR merge or CI success into publication/deployment.

Current charter language describes local-only Cadence. Cloud coordination is
being defined by CAD-525; this proposal does not rewrite the charter or invent
working hosted organization authorization. Explicit project memory retrieval
returned no matching accepted lesson for CAD-662/tracker/rollout at design time.

## Identity and version policy

Every lookup/write is bound to the selected connection authority namespace,
organization (remote only), project and optional stream. A standalone-local
connection uses its explicitly selected tracker identity, with `organization_id:
null`; it must not fabricate an AgenticOS organization. A remote connection uses
its authenticated issuer/organization, never only a renameable profile alias.
CAD-657 binds local runtime and tracker paths together; its organization label
is selection/display only. `authority_id` is a proposed stable export identity
for that actual authority, not a currently implemented cloud identity or alias.
Its allocation/verification remains a planning-writer prerequisite.
The stream is a product/component or maintenance-line version namespace, not a deployment environment.
`null` is the default stream; an empty string is invalid. Stable record IDs, never
display versions, name directories and references.

Uniqueness is `(authority_id, organization_id, project, stream, version_key)`, across archived
and all publication states and policy revisions. A release keeps its policy ID,
display version and canonical key permanently. Display title, notes and owner
can change through audit; renumbering means create a new planned release and
archive the old one. Same `1.0.0` in another project/stream/org is legitimate.

Policies are immutable, revisioned records selected per project/stream:
Versions/names are trimmed and bounded to 128 characters in the example validator.

- **SemVer 2.0:** validate full `MAJOR.MINOR.PATCH`, prerelease and build metadata;
  canonical release key excludes build metadata. Precedence follows SemVer;
  build metadata distinguishes builds, not separate release scope. `v` is a tag
  prefix, not part of the version. Historical product versions are not inferred
  from Cargo package versions. [SemVer specification](https://semver.org/spec/v2.0.0.html)
- **Calendar:** v1 supports exactly `YYYY.MM.PATCH`, UTC calendar year/month,
  with nonnegative numeric patch. Compare year/month/patch numerically. This is
  Cadence's explicit policy format, not a claim that all CalVer follows it.
- **Named:** nonempty trimmed NFC name, case-sensitive identity; no numeric
  precedence or compatibility inferred. Lists use target/publication dates and
  stable ID for deterministic ordering.

A policy change selects a new immutable policy for future releases and leaves
old policy references/keys unchanged. Cross-policy lists label the policy and do
not pretend that SemVer, calendar and named versions share one precedence.

## Records and relationships

| Record | Authority and contents |
|---|---|
| Release | Stable ID/context/policy/version; owner, target date, state `planned/candidate/published`, independent archive flag, notes/limitations, milestone refs, active candidate/publication IDs and revision |
| Issue link | Scoped issue ID/type plus `target` or `affected` and a release ID; exactly one active target per issue per stream, several affected links for bugs only |
| Build | Exact source set, immutable artifacts/digests and manifests; product/protocol/schema/image dimensions and declared compatibility constraints |
| Candidate | Exact release ID, scoped build IDs, proposed shipped issue IDs, notes/limitations snapshot and an exact review-evidence ref; replaced by new ID on drift |
| Publication | Exact candidate/build/shipped membership snapshot, publisher/time and verified GitHub or manual distribution evidence; immutable |
| Deployment | Exact release/build, environment, observed time, verification ref, prior deployment ID and rollback relationship; append-only |
| Audit event | Server-derived actor, action, affected record, previous/new revisions, time, reason and operation idempotency key |

Issues belong to the project resolved from the tracker, not a prefix guessed by
the caller. Planned target relationships can move to a later release under one
atomic writer operation with old/new targets and reason audited. Affected links
describe bugs, not fixes. Shipped membership is read from publication receipts,
never issue status or mutable target links. One bug fix can ship in a maintenance
and a mainline release; both receipts retain it. Concurrent planning uses explicit
mainline/maintenance streams, each with its own target; retargeting one stream
does not erase another's target. A stream is not inferred from a branch name.
Scope can be partly delivered:
explicitly publish the candidate's subset, explain exclusions, then retarget the
unfinished issues. New unplanned shipped issues must first enter audited target
scope before candidate preparation. Empty-scope releases are allowed only for
explicitly described non-issue deliverables, not automatically marked ready.

## Lifecycle, immutable evidence and authorization

Planned records allow audited scope/date/editorial changes. Candidate creation
pins a scope/build/notes snapshot; independent review binds to its digest and
exact source set. A later source, artifact or scope change invalidates active
candidate review and returns the release to planned; create a new candidate,
retain the old one. Merely changing a Git tag's target is drift, not a new receipt.

Publication is a separate evidence-backed operation, not a GitHub upload command.
It requires the authorized publisher and exact independently reviewed candidate,
its verified distribution evidence, and CAS on the release revision. Published
version/policy, shipped scope, source set, artifact digests, candidate snapshot,
publisher/time and receipts cannot be rewritten or removed. Corrections to
title/notes/limitations use an append-only editorial event with before/after and
reason, leaving the publication snapshot intact. A withdrawn or unavailable
upstream release gets a new observation, not deletion/reuse of the version.
Archive hides a record by default; unarchive restores visibility without changing
state/history. No hard-delete command is proposed.

The future writer must reuse tracker lock/revision/lease fencing and commit only
owned paths, rolling back preimages on failed commit. Proposed storage is
`<tracker>/<project>/releases/<stable-id>/` for release/receipt/audit files and
an index for policy/issue relationships; the organization is the verified tracker
connection boundary plus an explicit identity in each export. Do not read/write
one org's records through another connection merely because project keys match.
Legacy readers/writers do not display or manage release membership; they keep
their existing issue behavior without being a release writer. The new release
writer owns the ledger/index revision validation and must reject an unsupported
contract version rather than rewrite it. Organization switching selects a
connection, not a migration of records or running agents.
Publication receipts and their canonical digests need a protected writer-owned
authority anchor: plain editable tracker bytes or self-asserted actor/hash fields
are not proof. Pin the concrete anchor/signer/custody and retention mechanism in
the enforcement implementation before enabling publication. Until that design
and adversarial proof exist, publication/import mutations must remain disabled;
ordinary planning can ship independently. A git history mismatch or missing
verification reports unavailable/tampered, never reconstructs a success.

Actor identity and grants come from authenticated daemon/HTTP context, never a
JSON `actor`/`role`. Planning allowlist is operator plus an explicitly granted
project PM; publication, imports and deployment attestations require operator or
an explicitly granted publisher/observer capability. Owner is accountability,
not automatic permission. HTTP must enforce at least the same proof as the
daemon/CLI. Before implementing these gates, write adversarial tests for agent
callers, detached children, concurrent writes and forged fields, including HTTP
parity. This document adds no grant or exception to existing release authority.

## GitHub and manual evidence

GitHub import records canonical repo identity, upstream release ID, tag name,
resolved full source SHA, upstream publication time, URL and each asset's upstream
ID/name/size/digest. A verifier binds them to the candidate/build manifest and
successful publication evidence; a draft is not published. Idempotency key is
`(organization, project, stream, repo identity, upstream release ID)` plus snapshot
digest. Replay of identical evidence is a no-op; changed assets/tag/source on the
same upstream ID is a conflict/revision observation, never silent replacement.
Same tag on another repository is a different external identity. CAD-661 owns
creating the actual GitHub Release; this feature consumes its evidence.

Manual publication supports reports, campaigns and non-GitHub software. An
authorized publisher attests an exact candidate and immutable artifact/content
digests, distribution destination, actual time, limitations and a verification
ref (e.g. captured recipient-access evidence). Record a stable attestation ID,
verified attester identity and verifier receipt. It may say `attested`, not
`GitHub verified`; a URL or unchecked checkbox alone cannot authorize publication.
Synthetic fixture attestations are deliberately not cryptographic proof.

Deployments are separately attested observations. Their release/build must belong
to the same context, source/digests must match, and verification must describe
what was running. Record previous observation and, for rollback, the earlier
observation restored. A failed rollout attempt is an audit/operation outcome,
not an observed successful deployment. Runtime image, CLI product, remote
protocol and DB schema dimensions are recorded separately. Compatibility entries
state declared constraints and source; automatic enforcement is not implemented.

## Proposed interface and first implementation slice

The following syntax is reserved by this proposal only; none is supported yet.
Organization/connection selection is CAD-657, and running teams stay pinned.

```text
cadence release policy set --project p [--stream s] --scheme semver|calendar|named
cadence release new --project p [--stream s] --version VALUE --owner ID [--target DATE]
cadence release ls --project p [--stream s] [--state planned,candidate,published] [--archived] --json
cadence release show RELEASE-ID --project p --json
cadence release edit RELEASE-ID --project p --if-rev REV --file metadata.json
cadence release target ISSUE-ID --project p [--stream s] --release RELEASE-ID --if-rev REV --reason TEXT
cadence release target ISSUE-ID --project p [--stream s] --clear --if-rev REV --reason TEXT
cadence release affected add|remove ISSUE-ID --project p --release RELEASE-ID --if-rev REV
cadence release archive|unarchive RELEASE-ID --project p --if-rev REV
```

Planning writer/read model is the smallest implementation batch: immutable policy
revisions, planned record create/list/show/edit/archive, target/affected links,
revision conflicts and audited retargeting. Published history is read-only until
verified receipt writer exists. Build/candidate/publication/import/deployment
mutations follow as separately reviewed batches; proposed operation names are
`build record`, `release candidate`, `release publish`, `release import github`,
`deployment record`. No automatic external posting or production change occurs.

Board proposal: project navigation gains Releases list/detail; list shows
version/policy/stream/state/archive, owner, target/actual dates and readiness
evidence status. Detail separates planned scope, affected bugs, candidate builds,
shipped history, distribution evidence and environment observations. Show unknown
evidence as unknown, empty scope explicitly, and permissions before controls.
Create/edit and retarget dialogs submit the same revision and writer contract;
409 retains drafts and offers reload. Published immutable fields cannot become
editable through the board. Audited editorial corrections have a separate action.
First-slice HTTP proposals are project-scoped releases GET/POST/detail PATCH,
issue-link PUT/DELETE, and archive POST under `/api/projects/:project/releases`.
The authenticated org is outside caller-controlled path/body; all request fields
are allowlisted and every mutation carries revision/idempotency context. This is
an interface specification, not accepted product UI or a working endpoint.

## Acceptance and validation ownership

| CAD-662 criterion | Component/evidence needed after this contract increment |
|---|---|
| Scoped release metadata | Planning writer, isolated-org fixture, uniqueness/CAS tests and CLI/board checks |
| Target/affected/shipped separation | Ledger index/read model, retarget and backport receipts, real board/CLI workflows |
| Candidate and immutable provenance | Candidate snapshot builder, exact-source review, protected publication anchor, drift/tamper/adversarial tests |
| GitHub/manual publication | Verified adapters, authorized actor proof, identical-replay/conflicting-replay tests; actual CAD-661 evidence |
| Build/deploy/rollback distinction | Manifest reader and observer, separate deployment history; real environment probes |
| Version policies | Validated parsers/comparators, immutable policy revisions, collision tests across project/stream/org |
| Compatibility dimensions | Exact manifest/read model entries and truthful declared-only display; enforcement needs separate tests |
| Failure/isolation/authorization | Same CLI/daemon/HTTP adversarial matrix, caller identity proof, concurrent retarget/publish and no cross-org lookup/write |

`validate.py` checks the v1 schema/fixture and deterministic model examples using
Python plus `jsonschema`; it is not part of the production authorizer or a Rust
CI gate. Positive examples include partial scope, a backported issue, manual
publication and rollback. Negative examples cover collisions, target/affected
confusion, candidate drift, provenance rewrites and import replay conflicts.
No runtime authorization, signatures, database migration or installed daemon is
tested. Native implementation tests remain admitted CI/build-slot work.
The example audit test proves that a matching append-only event accompanies a
revision; it does not prove actor permission, correct event action or custody.
Those require the future shared writer and its authenticated grants.
