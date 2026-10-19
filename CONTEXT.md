# Cadence work

Cadence coordinates people and agents doing work through apps and projects.

## Language

**Workspace**: The ownership and access boundary for a person's or organization's work. A workspace contains app installations and projects.

**App**: A reusable capability with its own workflows and user experience, such as Social Content or Open Slide.

**App installation**: A workspace's configured instance of an app, with its own identity, team defaults and approved capabilities. Updating the app does not change who owns its work.

**Catalogue release**: An optional locator of catalogue id, version and digest recorded on an installation after that exact package matched directory metadata. The locator is not trust or permission, and publishing or disabling a release does not change an installation that was already recorded.

**App context**: An optional named context within an app installation that keeps a client's or brand's settings, records and connection bindings separate. A context does not grant access by itself.

**Project**: An optional grouping of related work, potentially spanning multiple apps and repositories. Linking work to a project does not transfer ownership or grant access.

**Run**: One execution of an app workflow, with its inputs, assigned team, progress and outputs. A run belongs to an app installation and may name an app context and a project.

**Destination**: Where a run's output is delivered, such as a repository, an outbox or a social account. A destination is not the owner of the app.

**Connection**: An explicitly authorized route to an external or local capability. Using a connection requires permission for the particular action and context.

**Provider account**: The identity a connection uses at a particular provider. Its meaning is provider-specific: a social destination account is not an AgenticOS billing workspace.

**Local agent**: An agent executing on the user's machine. Dispatching work to a local agent does not request execution inside a hosted container.

**Hosted coordination**: Shared tickets, assignments, messages, knowledge and results maintained for a workspace. The location of this coordination does not determine where an agent executes.

**Assignment**: A delegation of work to a workspace member and their chosen agent. An assignment can wait for a local agent; assignment alone does not prove execution or process termination.

**Global Wiki**: The team's shared knowledge bucket. Authorized team members collaborate there; shared does not mean anonymously or internet-publicly accessible.

**Published collection**: An owner-controlled selection of knowledge revisions with source, applicability and accountability metadata. Publishing does not grant access, and a later collaborative edit does not inherit an earlier revision's approval.

**Knowledge correction**: A member's reported question, answer, cited revisions, expected answer and reason awaiting source-checked review. A report is not source authority.

**Reviewed regression**: A correction accepted by an independent authorized reviewer and bound to the checked source revisions. Changed sources do not silently change the accepted case or make it current.
