# Shareable artifact previews — proposed contract (CAD-656)

Status: architecture and future acceptance plan, not implemented or security
validated. This P2 feature remains outside CAD-525's October 9 cloud milestone.
No infrastructure, upload, share endpoint or production action is introduced here.

## Existing storage: what we can reuse

AgenticOS source inspected at
[`31dbcef0ae36fa25fdd613a6fb31283a4652f1a1`](https://github.com/agenticos-stack/agenticos-v2/tree/31dbcef0ae36fa25fdd613a6fb31283a4652f1a1).
These are source observations, not verification of deployed configuration:

| Existing boundary | Evidence at that SHA | Consequence |
|---|---|---|
| Logical company filesystem | [files.ts](https://github.com/agenticos-stack/agenticos-v2/blob/31dbcef0ae36fa25fdd613a6fb31283a4652f1a1/apps/api/src/files.ts) resolves membership, lists/reads nodes and stores mutable text; non-text uploads are refused | Reuse org/workspace identity and explorer references, not an imaginary existing binary publisher |
| Filesystem metadata | [db/schema.ts](https://github.com/agenticos-stack/agenticos-v2/blob/31dbcef0ae36fa25fdd613a6fb31283a4652f1a1/apps/api/src/db/schema.ts) defines workspace_files with revisions, body, objectUrl and intrinsic flags | An immutable label/reference does not establish immutable storage custody |
| Runtime state | [runtime/store.ts](https://github.com/agenticos-stack/agenticos-v2/blob/31dbcef0ae36fa25fdd613a6fb31283a4652f1a1/apps/api/src/runtime/store.ts) serves COMPANY_STATE under companies/&lt;company&gt;/; GET/HEAD/PUT/DELETE require current instance lease | Mutable snapshots and externally shared versions must not use the same writable prefix |
| Container authority | [runtime/stub.ts](https://github.com/agenticos-stack/agenticos-v2/blob/31dbcef0ae36fa25fdd613a6fb31283a4652f1a1/apps/api/src/runtime/stub.ts) injects company/instance into store.internal; [state-bridge.sh](https://github.com/agenticos-stack/agenticos-v2/blob/31dbcef0ae36fa25fdd613a6fb31283a4652f1a1/infra/runtime-image/state-bridge.sh) updates snapshot.tgz and database replication | Reuse host-derived tenant identity, never trust a submitted org or container placeholder key |
| Storage binding | [wrangler.jsonc](https://github.com/agenticos-stack/agenticos-v2/blob/31dbcef0ae36fa25fdd613a6fb31283a4652f1a1/apps/api/wrangler.jsonc) declares COMPANY_STATE R2 and COMPANY_CONTROL | Durable bytes already exist; this does not configure a private artifact bucket/share gateway |

“Org” in the user-facing design maps to the platform's stable workspace/company
ID, not its mutable display name or slug. The runtime prefix tenant and filesystem
membership must resolve to that same ID. Existing workspace role resolution and
container attribution remain the authority for publication callers.

## Ownership and publication

```text
org filesystem /projects/<project>/artifacts/<artifact>/<version>/
                    → immutable manifest reference
local/cloud publisher → private staging → verified published objects
external browser     → preview Worker → live share authority → private objects
                                       (no container lease or wake)
```

Use a separately protected private artifact bucket for the first implementation.
The container's store.internal bridge must have no access to it. Reuse R2 and
the validated tenant-key pattern; do not store published objects anywhere under
companies/<company>/ while that holder can PUT/DELETE arbitrary keys there.
No public bucket/custom-domain origin, r2.dev URL or direct presigned download
may bypass share authorization. A shared bucket would require a separately
reviewed custody restriction; a new prefix alone is insufficient.

Proposed records, durably owned by a per-org artifact authority outside Cadence:

| Record | Required identity/lifecycle |
|---|---|
| Publication | org, artifact, immutable version, request ID, staging generation, state, manifest digest, creator |
| Manifest | canonical relative paths, sizes, SHA256 digests, validated MIME, entrypoint; no symlinks or ambient object URLs |
| Filesystem reference | org/file node → publication ID + manifest digest; cannot alter published bytes |
| Share | org, publication, share ID, signing kid, read scope, expiry, state/revision, creator/time |
| Browser session | share ID/revision, digested random session credential, expiry; no workspace permission |

Default policy: owners manage external sharing/revocation/deletion. Publishing
requires an explicit org/project `artifact:publish` capability; delegated agent
credentials cannot acquire it by forging actor/role/org fields. Members/viewers
do not gain external-sharing authority merely because they can read/edit files.
These are proposed allowlists requiring role-contract review before implementation.

Publication state is staging → verifying → published, or failed; deletion is
an authoritative tombstone followed by eventual byte cleanup. Allocate unique
staging keys; accept only bounded regular files with collision-free canonical
paths. The trusted publisher computes digests and verifies every object before
committing a manifest. Final objects are create-only, scoped to org/version;
overwrites with different bytes fail. Immutability means all write paths and
cleanup authority are controlled, not just a filename containing a hash.

The authority atomically commits the verified manifest and published state only
after every referenced object is durable. R2 does not supply a cross-object
publication transaction. A crash before that commit leaves an invisible orphan;
retry by the same request ID resumes or returns the same outcome. A crash after
commit leaves a readable complete version. Filesystem-reference repair is an
idempotent outbox step; a dangling reference cannot make staging bytes public.
Orphan cleanup must consult committed references and active publications before
deletion. Deletion tombstones deny reads/shares before byte/cache cleanup.

Recommend a dedicated per-org Durable Object authority for serialized publication,
share/session admission and revocation. It has no container lease dependency and
no wake side effect. This is a proposal, not an existing binding. D1 may hold
search/explorer indexes, but a stale index cannot authorize reads; eventually
consistent KV or edge-cached share state cannot be the immediate revocation gate.

## Signed links and nested assets

Proposed browser flow: unique share host on a separate preview **site** from
AgenticOS/Cadence, e.g. `s-<random-id>.preview.example/#s=<signed-token>`.
The signed claim contains share ID, org, immutable publication, audience, expiry,
kid and revision. The fragment is not sent in the initial HTTP request. A trusted
bootstrap removes it using history.replaceState, then POSTs it to the exchange
endpoint in a body excluded from logging. Validate signature with an explicit
algorithm/key allowlist and check current authoritative share/publication state.

Exchange mints a bounded host-only Secure/HttpOnly session cookie, with no Domain
attribute, and presents token-free content URLs. The unique share host keeps
cookies and paths from granting access to another share. Session lifetime cannot
exceed link expiry. Cookie SameSite/partitioning and sandboxed asset delivery
need proof on supported browsers; if delivery is blocked, fail closed rather
than relaxing HTML isolation. Exchange and mutation endpoints require exact origin/host
checks; never reflect submitted redirect/origin values. The signed link remains
a bearer credential: forwarding it grants access while valid. Fragment handling
reduces routine request/referrer exposure; browser history, extensions, copied
links and compromised bootstrap code still require threat review.

Untrusted HTML receives neither the signed token nor the session cookie value.
It loads only manifest-listed assets through the gateway using the scoped cookie;
the same authorization applies to every GET/HEAD, download, conditional request
and range request. A copied asset URL without a valid session fails. Non-browser
downloads may use an Authorization header, never per-asset query tokens or a
direct private-origin URL. No token in asset paths, analytics or cache keys.

Every new gateway admission checks session/token validity, live share revision,
expiry, published/non-deleted state and manifest membership **before** a cache
lookup. Malformed/traversal/double-encoded paths, unknown files, cross-org keys,
wrong hosts and unsupported methods fail closed. Range/If-None-Match/HEAD requests
do not bypass admission. Authoritative state unavailable means no content served.
Return a generic unavailable page without org/project metadata for denied shares.

## Revocation, caching and sleep

Revocation is durable before acknowledgement. A request admitted after that
acknowledgement is denied, including a warmed cache hit, 304 or 206 response.
Concurrent admission/revocation is serialized by the authority: a previously
admitted stream may finish; already delivered bytes cannot be recalled. Expiry
also applies at admission. Any stronger active-stream cancellation promise needs
separate implementation and measured acceptance evidence.

Send `Cache-Control: private, no-store` on all client responses, including errors,
bootstrap/exchange and assets. This discourages compliant browser caches; it
cannot erase copies already downloaded, screenshots or previously cached bytes.
Artifact deletion/revocation means denying **new gateway access**, not remote
erasure of a viewer's machine. Retention/deletion is separate from link expiry.

An optional server-side byte cache uses tenant + immutable manifest digest +
asset digest keys, behind admission. Construct a separate internal cache response;
never cache cookies or authorization decisions and never expose that cache route.
Cached bytes may be physically purged on deletion but authorization must not
depend on successful purge. No CDN rule may serve preview URLs before the Worker.
No GET/HEAD/asset request invokes a container, board relay, runtime lease or wake;
publication may need a live producer, serving a committed version does not.

R2 object operations are strongly consistent; public custom-domain caches can
continue serving deleted/old objects. That is why private binding access and
gateway admission are required. [R2 consistency](https://developers.cloudflare.com/r2/reference/consistency/)
The Worker API supports conditional writes and ranged reads, but each still needs
the application contract above. [R2 API](https://developers.cloudflare.com/r2/api/workers/workers-api-reference/)
Worker caches are separate serving mechanisms, not revocation authority.
[Cache API](https://developers.cloudflare.com/workers/runtime-apis/cache/)

## Untrusted HTML and crawler controls

The trusted viewer embeds HTML in a sandboxed iframe; artifact responses also
carry an enforced CSP sandbox. Default is static content; a reviewed interactive
mode may allow scripts without allow-same-origin, top navigation, popups, forms,
service workers or Cadence API access. Use an explicit CSP allowing only needed
manifest assets on that exact share host, with external connections blocked.
Do not combine allow-scripts and allow-same-origin for artifact content. Validate
nested CSS URLs, script imports, SVG, workers and iframe navigation in real browser
tests rather than treating a header string as proof. Unsupported active types
are downloads or rejected. [CSP sandbox](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Content-Security-Policy/sandbox)

Use `Referrer-Policy: no-referrer`, no third-party analytics and no permissive
credentialed CORS. Platform, edge, error, tracing, WAF and upload logs must redact
Authorization, cookies, exchange bodies and any accidental share-token URL.
Record share ID and outcome only; test this with planted canary credentials.

Send `X-Robots-Tag: noindex, nofollow, nosnippet` on pages/assets and add no public
listings or sitemaps. Known-crawler blocking, rate limits and optional challenge
are best effort; no promise of bot-proof links or zero indexing. Unfurlers lacking
the fragment/session see generic content and never consume/revoke a share.
robots.txt is not access control and can prevent crawlers observing noindex.
[Google noindex guidance](https://developers.google.com/search/docs/crawling-indexing/block-indexing)

## Negative acceptance matrix — tests to implement, not passing evidence

| Attack/failure | Future observable acceptance |
|---|---|
| Agent/member/viewer forges owner, org or permission; detached local child; alternate HTTP path | Publish/share denied by allowlist; HTTP no weaker than daemon. Remove guard to demonstrate attack test fails |
| Container holder PUT/DELETE against published custody | Published bytes unreachable through store.internal, including traversal; digest/key protection denies overwrite |
| Missing/truncated asset, symlink, duplicate normalized path; crash at each publish stage | No visible partial version; request-ID retry gives one complete manifest; GC cannot delete referenced bytes |
| Tampered/unknown-kid/wrong-audience token; wrong host/org/version; fabricated filesystem objectUrl | Denied without tenant metadata or origin URL; no signed-token-only bypass of live share state |
| Asset URL copied to fresh browser; nested CSS/script/media, HEAD/304/range/unsupported range | Every route requires same share admission; no unsigned or cross-share assets; invalid range refused after authorization |
| Revoke/delete/expire after cache warming; requests race with revocation | Post-ack admissions return no bytes, 304 or 206; documented pre-admission streams may finish; private cache cannot bypass gate |
| Share-authority outage or stale index | Fail closed even with warmed bytes; no cached authorization fallback |
| Company sleeping, stopped or without lease | Published version serves; runtime wake counters and lease calls remain unchanged |
| Hostile HTML tries parent navigation, cookie/API reads, other shares, external requests, service worker | Browser proof of isolated authority/network restrictions; no signed token in artifact DOM or requests |
| Canary credential in link/exchange/cookie/header; external navigation, denial logs, analytics | No raw credential in telemetry/referrers; no-store on all responses; document already-downloaded-copy limitation |
| Known crawler/unfurler; arbitrary automation imitates browser | Known agents blocked/generic where configured, headers present, shares not consumed; no absolute automation/indexing claim |

Next implementation ownership: AgenticOS storage/share authority and preview Worker;
Cadence snapshot producer and logical filesystem/board UX; independent reviewer
for role/custody/browser proofs. Select those owners and quotas/retention/signing
rotation policies before implementation. CAD-656's eight delivery checks remain
unchecked until real implementation and independent evidence satisfy them.
