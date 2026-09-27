# AgenticOS host lease adapter (CAD-673)

The host grants a company container its writer claim before starting it.
Cadence can verify that existing claim using `POST http://lease.internal/renew`.
The internal host route supplies company and instance from provisioning context;
Cadence supplies no authentication header, cookie, identity fields or request
body. This is container identity, not browser/CLI login or a remote agent token.

`hosted.lease` accepts `http://lease.internal`, its trailing-slash form, or the
exact `/renew` URL. All normalize to the same renewal URL. Foreign hosts, HTTPS,
ports, userinfo, alternate paths, queries and fragments refuse before network
access or state writes. Redirects and ambient HTTP proxies are disabled.
Loopback URLs exist only in private unit fixtures, with no configuration override.

First `204` admits the daemon before its store opens. Requests have a two-second
total timeout, reduced to the remaining local renewal budget. `409` is definite
lease loss. Every other status or transport error also permanently refuses this
provider, preserving Cadence's existing first-renewal-failure policy. Responses
and generation-looking headers are not parsed as authority. A later successful
response cannot revive a failed provider or an expired local deadline.

Host renewals establish a monotonic six-second local deadline, measured from
request start. This is the bridge's existing local fencing policy, not an expiry
issued by CompanyControl. Shorter configured TTLs are allowed; values above six
seconds refuse. The default heartbeat is two seconds; an explicit heartbeat must
remain below the configured TTL. Diagnostic `expires_unix` approximates the same
deadline; host write checks use monotonic time. HTTP release does nothing: only
the host controls revocation and replacement.

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

## Hosted activation remains blocked

This adapter does not change the AgenticOS image's lease ownership guard.
`hosted.sh` still refuses enabled `hosted.lease` because `state-bridge.sh` remains
the only renewal owner. Do not enable the adapter in that image yet.

A joint Cadence/AgenticOS transition must prove exactly one renewal owner remains
alive through daemon shutdown, explicit final Litestream synchronization, and
the final tracker/files snapshot. Today the daemon joins its heartbeat before
its local flush; the bridge renews through all final uploads. Removing bridge
renewal without that handoff would leave final persistence work uncovered.
Required activation evidence includes startup admission, replacement/stale
instance fencing at the storage gateway, no competing heartbeat loops, slow and
failed final uploads, bounded SIGTERM, and truthful durable-flush success/failure.
AgenticOS retains host claim/revoke authority throughout.

The adapter tests cover actual HTTP transport, response forgery, redirects,
timeout, permanent loss, deadline forgery and concurrent shared store/tracker
fences. Existing `tests/hosted_lease.rs` retains process-level daemon shutdown and
file-provider regressions. No hosted deployment, native-agent reenrollment,
cross-container persistence proof or complete migration acceptance is asserted.
