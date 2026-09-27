---
status: proposed
issue: CAD-662
---

# Keep project release history separate from issue and milestone rewrites

Store release planning, immutable candidate/publication snapshots and deployment
observations in a versioned project release ledger, with issue target/affected
relationships owned by that ledger. Existing issue rewrites are lenient and can
drop unknown frontmatter, so embedding authoritative shipped membership in issue
files would make history vulnerable to older writers and backports ambiguous.
Milestones keep their outcome model; builds and rollout receipts remain distinct
evidence sources. Publication authorization and tamper detection require verified
writer/provenance enforcement, not merely a git commit author or JSON field.

This proposal trades another project record/index for preserved immutable
delivery history and no issue-frontmatter migration in the first implementation
slice. It does not introduce a runtime writer or replace the rollout lease.
See the [v1 contract](../design/project-releases-v1/CONTRACT.md).
