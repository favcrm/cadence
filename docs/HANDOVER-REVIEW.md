# Handover review and prototype transition

The local agent-handover skill is now included in this repository for review.
It remains a same-host artifact and mailbox workflow, separate from the Rust
controller's managed delivery API.

## Fixed

- Same-second publications keep the strict type suffix and distinct chain times.
- An old verdict no longer closes a subsequently reopened chain for pruning.
- Ambiguous legacy same-second endings are retained conservatively.
- Index generation shares the notes lock so pruning cannot remove files mid-read.
- Instructions distinguish publication, submission, agent ACK and verified work.
- Native-session identity, endpoint ownership and working return routes are explicit.
- Removed a blanket UI-approval requirement from the handoff template; existing
  user scope and authorization govern the workflow.

## Validation

Four temporary-directory regression tests cover concurrent publication,
closed-chain pruning, reopened-chain retention and legacy time ambiguity.
The skill frontmatter validator passes. The Python prototype's full 47-test
baseline passed before migration. Code-simplifier review: kept changes local,
reused existing locks, and retained the strict filename contract.

## Cleanup

Expired cooperative kickoff documents moved to a private local archive.
Historical README guidance is marked, and an obsolete claim that the parent
Codex chat cannot receive messages has been corrected. A single offline test
runner groups managed, bridge and legacy suites. Working return bridges,
source tests and private evidence are retained until Rust parity passes.
Nothing under private runtime state is included here.

## Remaining limits

File mailbox delivery is at-least-once and is not itself a wake mechanism.
The scripts depend on GNU tools and flock. Cadence provider commands remain
under development; this skill does not advertise proposed commands as working.
