# Security policy

Cadence runs on the operator's own machine: it drives coding-agent CLIs, keeps a
local board, and installs and updates itself from published releases. This policy
covers the places where a flaw crosses a trust boundary rather than merely
breaking a feature.

## Reporting a vulnerability

Use **GitHub private vulnerability reporting** — this repository's Security tab →
*Report a vulnerability*. It keeps the report private to the maintainers and gives
us one thread to fix and coordinate disclosure in. If that button is absent, mail
**dev@favcrm.io** with the same content.

Please do not open a public issue for a suspected vulnerability.

A useful report says: the release or commit you tested, the platform and how
cadence was installed, what an attacker gains, the smallest reproduction you have,
and whether any part of it is already public. Test only against systems you own —
do not run a live test against someone else's board, host or release store.

## In scope

- **Secrets and redaction** — credential material reaching logs, board payloads,
  transcripts, memory or release artefacts; the redaction paths in `src/secret/`
  and the vendored rule pack; anything that defeats a documented scrub.
- **The operator and agent boundary** — how a caller is derived and proven
  (`src/peer.rs`, `src/daemon/identity.rs`), operator-only verbs and approvals, the
  master permission broker, agent-uid provisioning, and the setuid-root drop helper
  `cadence-agent-exec`.
- **Remote paths** — device login, remote enrollment, the remote CLI, and the
  tailnet proof.
- **The board** — session lifetime and binding, the private-session boundary,
  cross-origin behaviour, and app or screen authorization.
- **Release and update integrity** — the published artefacts, their checksums and
  build attestations, `install.sh`, and `cadence upgrade` and rollout.
- **Gate bypasses** — a way to make a merge, a release or a promotion proceed
  without the check or the approval that is supposed to gate it.

## Out of scope

- **Provider CLIs and their own credentials.** Claude, Codex, Cursor, Devin and the
  rest are external dependencies addressed as they are; report their issues to
  their vendors. Cadence's own handling of a provider credential *is* in scope.
- **Third-party dependencies.** Report those upstream; a Dependabot alert on this
  repository is the fastest route to us acting on it.
- **Anything that presumes the attacker already has the operator's uid, shell or
  GitHub token.** That boundary is documented rather than defended —
  `docs/AUDIT.md` records the known same-uid residuals, and they are not
  vulnerabilities by themselves. A way to *reach* that uid from a
  lower-privileged position is in scope.
- **Self-inflicted load or cost** — an operator or agent running its own stress
  loop, or spending provider quota on their own machine.

## Supported versions

Cadence is pre-1.0 and moves fix-forward. Only the newest published release is
supported, so reproduce on the latest release before reporting; a fix normally
ships in the next one.

## What to expect

Best effort from a small team: we acknowledge the report, say whether we consider
it in scope, and then either ship a fix in the next release or explain why we are
refusing it. There is no bug bounty, and no NDA or embargo process — if you need a
coordinated disclosure date, say so in the report and we agree one.
