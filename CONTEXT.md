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
