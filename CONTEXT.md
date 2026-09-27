# Cadence work

Cadence coordinates people and agents doing work through apps and projects.

## Language

**Workspace**: The ownership and access boundary for a person's or organization's work. A workspace contains app installations and projects.

**App**: A reusable capability with its own workflows and user experience, such as Social Content or Open Slide.

**App installation**: A workspace's configured instance of an app, with its own identity, team defaults and approved capabilities. Updating the app does not change who owns its work.

**App context**: An optional named context within an app installation that keeps a client's or brand's settings, records and connection bindings separate. A context does not grant access by itself.

**Project**: An optional grouping of related work, potentially spanning multiple apps and repositories. Linking work to a project does not transfer ownership or grant access.

**Run**: One execution of an app workflow, with its inputs, assigned team, progress and outputs. A run belongs to an app installation and may name an app context and a project.

**Destination**: Where a run's output is delivered, such as a repository, an outbox or a social account. A destination is not the owner of the app.

**Connection**: An explicitly authorized route to an external or local capability. Using a connection requires permission for the particular action and context.

## Project delivery

**Milestone**:
A project outcome with explicit exit criteria and a target date. Its achievement
is separate from publishing a release or changing an environment.
_Avoid_: Release, version

**Release**:
A named delivery of a project's changes within an optional product stream.
A published release preserves its actual shipped scope and provenance.
_Avoid_: Milestone, build, deployment

**Release stream**:
A separately versioned product, component or maintenance delivery line within a project.
_Avoid_: Environment, branch

**Target release**:
The one release within a stream currently intended to deliver an issue. It is a planning
relationship and is not evidence that the issue shipped.
_Avoid_: Fixed version, shipped version

**Affected release**:
A release identified as exhibiting a reported bug. A bug may affect several
releases independently of where its fix is planned or shipped.
_Avoid_: Target release

**Shipped membership**:
The issues recorded as actually delivered in a publication receipt. One issue
can appear in multiple receipts when delivered through backports.
_Avoid_: Completed issues, planned scope

**Build**:
An exact set of source inputs and immutable deliverables prepared for a release.
Different builds may exist for the same planned release.
_Avoid_: Version, deployment

**Candidate**:
An immutable proposal binding a release's intended delivered scope to exact
builds and review evidence. A replacement candidate does not rewrite its history.
_Avoid_: Published release

**Publication receipt**:
Evidence-backed confirmation of a release's actual distribution, shipped scope
and provenance at a recorded time.
_Avoid_: Merged PR, successful CI

**Deployment observation**:
A record of the release/build verified in a particular environment at a time.
A rollback creates another observation rather than changing publication history.
_Avoid_: Published release, desired version
