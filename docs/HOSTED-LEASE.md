# AgenticOS host lease adapter (CAD-673, CAD-702)

The host grants a company container its writer claim before starting it.
Cadence can verify that existing claim using `POST http://lease.internal/renew`.
The internal host route supplies company and instance from provisioning context;
Cadence supplies no authentication header, cookie, identity fields or request
body. This is container identity, not browser/CLI login or a remote agent token.

`hosted.lease` accepts `http://lease.internal`, its trailing-slash form, or the
exact `/renew` URL. All normalize to the same renewal URL. Foreign hosts, HTTPS,
ports, userinfo, alternate paths, queries and fragments refuse before network
access or state writes. Redirects and ambient HTTP proxies are disabled.
Loopback URLs are reachable only through the in-process test-only endpoint
override (`ServeOptions::lease_http_endpoint_override`); no pm.yaml value,
environment variable, or RPC can select them. The configured spec is still
parsed first, so endpoint restrictions hold on what operators configure.

First `204` admits the daemon before its store opens. Every renewal attempt
keeps a budget of at most two seconds, reduced to the remaining local
renewal budget. `409` is definite lease loss: it latches at once, trips the
write fence, and the provider never contacts the host again. Any other
failure — a 5xx, a timeout, a transport blip — retries while the existing
six-second local deadline from the last `204` remains open: the failure
surfaces without fencing, and a later `204` inside the deadline heals and
clears the retry state. Only a deadline with no `204` fences. Responses
and generation-looking headers are not parsed as authority. A later successful
response cannot revive a latched provider or an expired local deadline.

Host renewals establish a monotonic six-second local deadline, measured from
request start. This is the bridge's existing local fencing policy, not an expiry
issued by CompanyControl. Shorter configured TTLs are allowed; values above six
seconds refuse. The default heartbeat is two seconds; an explicit heartbeat must
remain below the configured TTL. Diagnostic `expires_unix` approximates the same
deadline; host write checks use monotonic time. HTTP release does nothing: only
the host controls revocation and replacement.

Health tells retry apart from loss: while blips are outstanding inside an open
deadline, health reports `retrying: true` with `last_renew_error` naming the
latest blip; a `204` clears both, and once fenced `retrying` reads `false` —
the heartbeat has stopped, so nothing is still retrying. File leases report
neither key. The fence record (`lease-fence.json`) lands only on permanent
loss, never on a blip.

## Generation compatibility

AgenticOS does not expose its writer generation through this endpoint. Held
leases and `LeaseCtl::epoch()`/`PmLease::epoch()` now return `Option<u64>`:
host-bound leases use `None`, and file leases use `Some(epoch)`. This changes
the Rust source API for consumers of those public types.

Health and fence diagnostics serialize an unavailable epoch as JSON `null`;
health includes `epoch_available: false` and
`expiry_authority: "local_renewal_deadline"`. File leases retain numeric health
epochs and `expiry_authority: "provider"`. Consumers must not coerce null to zero
or infer a host generation from local holder labels.

HTTP-backed tracker commits omit `Lease-Epoch:` in normal commits and shutdown
flushes. A local holder label is diagnostic only. HTTP admission neither creates
nor overwrites `lease-epoch`; existing file-generation history remains available
if the state later returns to a file provider. The file provider's numeric record
schema, monotonic generations, fencing and epoch trailers are unchanged.

## Shutdown renewal (CAD-702)

`cadence daemon stop` keeps exactly one renewal poster — the heartbeat —
running through the WAL checkpoint and the tracker flush. The heartbeat is
stopped and joined only after the flush completes, and the lease releases
last, so there is never a window with zero posters (heartbeat dead while
the flush still runs) or two (a late renew racing the release). A fenced
daemon's tracker flush still refuses like every other write; the WAL fold
flushes what it committed while it still held the lease.

The ordering is proved by `cad702_http_renewal_continues_through_slow_flush`:
a stub host recording every POST time across a deliberately slow flush
(`ServeOptions::flush_delay_for_test`, in-process fixtures only), asserting
renewals span the whole shutdown window and go silent once the daemon is
gone. `cad538_stolen_lease_refuses_the_shutdown_flush` proves a lease lost
mid-shutdown still refuses the flush. No file/fd signal or exit-status
handoff was needed: the daemon itself remains the single renewal owner
until its final write, so an external owner takes over only by acquiring
the lease after this process releases it.

## Hosted activation remains blocked

This adapter does not change the AgenticOS image's lease ownership guard.
`hosted.sh` still refuses enabled `hosted.lease` because `state-bridge.sh` remains
the only renewal owner. Do not enable the adapter in that image yet.

A joint Cadence/AgenticOS transition must prove exactly one renewal owner remains
alive through daemon shutdown, explicit final Litestream synchronization, and
the final tracker/files snapshot. The daemon now renews through its own local
flush and releases last; the bridge still renews through all final uploads on
the AgenticOS side, so enabling both would put two posters on the lease.
Removing bridge renewal without that handoff would leave final persistence work
uncovered. Required activation evidence includes startup admission,
replacement/stale instance fencing at the storage gateway, no competing
heartbeat loops, slow and failed final uploads, bounded SIGTERM, and truthful
durable-flush success/failure. AgenticOS retains host claim/revoke authority
throughout.

The adapter tests cover actual HTTP transport, response forgery, redirects,
timeout, blip-then-heal inside the deadline, immediate 409 fencing, deadline
fencing with no further host contact, bounded stalled attempts, deadline
forgery and concurrent shared store/tracker fences. `tests/hosted_lease.rs`
adds the stub-host shutdown-ordering proof above and retains process-level
daemon shutdown and file-provider regressions. No hosted deployment,
native-agent reenrollment, cross-container persistence proof or complete
migration acceptance is asserted.
