# Hosted AgenticOS Pi workers

Hosted startup supplies AgenticOS's model catalog to the master. Pi workers
use separate private configuration directories, so they need their own catalog
before Pi starts. For the explicit model `agenticos/z-ai/glm-5.3-flash`, Cadence
checks the daemon's configured `models.json` against its reviewed AgenticOS
provider definition and writes only that definition to the worker's private
`models.json` at mode 0600. Every open refreshes this daemon-owned file.

The reviewed definition uses `http://gateway.internal/v1`, OpenAI-compatible
completions, and the literal `agenticos-placeholder` key. No operator login,
other provider configuration, headers, environment-based key expression or real
API key is copied through this path. Other worker providers retain their existing
authentication behavior. A missing, oversized, symlinked or changed source
catalog, or an unsupported AgenticOS model, refuses before a Pi child launches.

A worker directory with existing credentials is also refused without deleting
or rewriting its login. Use a fresh worker directory when switching from another
provider. Pi's normal empty `{}` auth store is allowed for resume; malformed,
nonempty, oversized or symlinked auth stores are refused. Store values never
appear in errors or the generated model catalog.

The existing operator model allowlist still has to admit the model. This change
does not alter that policy or authorize a paid turn. Pi's reported model and
effort remain verified at startup; confinement remains enabled as configured.
The gateway continues to derive tenant identity from provisioning and validate
the current lease. Its upstream key remains server-side. This internal route
requires a hosted runtime; it is not an external inference API for local workers.

Catalog cost fields are SDK display placeholders, not a promise of free
inference. Live requests use the company's wallet and server billing policy.
Writer and reviewer must be different assigned agents, and the Social Content
run and its capabilities still need their normal approvals. Installing this
change alone does not verify a caption, image, independent review or local draft.

The narrow catalog is pinned to the reviewed runtime definition. A future model
or gateway configuration change requires a coordinated source review rather
than copying arbitrary `models.json` contents. Runtime image adoption and image
media activation remain with AOS-119; this fix does not deploy or restart it.
