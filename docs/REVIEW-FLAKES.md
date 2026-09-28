# Review flake sightings

`cadence review flakes` prints the saved sightings as JSON lines.
`cadence review flakes --test module::test` selects an exact test name.
Use the global `--state-dir` to query an isolated state directory. The query
never appends, creates a ledger or starts a review. A missing ledger is empty;
malformed lines are ignored. Historical rows retain their original fields.

New sightings store repository keys as lowercase `owner/name`. GitHub HTTPS,
HTTP and SSH clone URLs normalize to the same key. Historical rows with no
repository (or null) match any repository for that test; invalid explicit
repository keys never match a valid repository.

Known-flake classification requires three distinct nonempty heads observed
within the inclusive last 14 days. Future, invalid and missing timestamps do
not qualify. Old rows remain visible and count toward the historical sighting
total, but cannot exempt a current failure from blocking review. The current
sighting participates under the same timestamp policy. This policy does not
change the equal-conditions isolated failure checks or review authorization.

Writers acquire the ledger lock nonblockingly with a five-second deadline.
Contention timeout reports an error without appending. The kernel releases the
lock when its file descriptor closes. Queries read saved bytes without taking
an exclusive lock; an incomplete concurrent append is ignored until complete.

Before a full `cadence review`, obtain build/test admission and set
`CADENCE_SUITE_LOCK` to the host's designated suite lock path. These are
separate requirements; see [CONTRIBUTING.md](../CONTRIBUTING.md).
