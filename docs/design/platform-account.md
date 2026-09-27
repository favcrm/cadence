# CAD-619 implementation contract and open dependencies

The latest ticket comment (2026-09-26 19:44 UTC) overrides older owner-action proposals: Cadence renders read-only account information. Account mutations remain on the AgenticOS app origin. No platform cookie or credential is forwarded through Cadence.

## Verified upstream reads

Inspected AgenticOS local branch `feat/account-api`, head `f8c2155fbc31be8f2041cdb30ed02e63fb0c5bcc`, through `git show` in `/home/ubuntu/Project/agenticos-stack/worktrees/agenticos-v2-platform-account`:

- `apps/api/src/runtime/account.ts`: fixed company/instance provisioning context, active lease check, GET `/v1/runtime/account` and `/v1/runtime/account/usage` only.
- `packages/contracts/src/account.ts`: `{ok,data|error}` envelope; HKD scale-six decimal strings; nullable plan; usage rows with date, description, signed amount, kind.
- `docs/architecture/account-api.md`: runtime account surface explicitly does not list members and never accepts mutations. Runtime membership routes return 404. Its account link still points to an older billing path, so Cadence discards platform links and constructs the newly required app account URL from a validated hosted board slug.

The Cadence read projection uses fixed `http://api.internal` URLs only when PublicBoard has the `http://api.internal` issuer. It performs no redirects, forwards no browser headers/cookies, bounds each request to three seconds and 64 KiB, limits displayed usage to 20 rows, validates a safe projection and discards unknown fields. Self-hosted boards have no account tab. Hosted error states retain a bounded retry through Refresh.

Manage account is a top-level new-tab anchor to `https://app-v2.agenticos.hk/account?company=<slug>` with noopener/noreferrer. Slug comes only from the configured `*.cadencecloud.app` host. No guessed workspace-id-to-slug conversion is made for other hosts.

## Dependencies that remain unresolved

Member display cannot be wired credential-free against the inspected upstream contract. The panel explicitly says it is unavailable. Upstream must define and approve an appropriate read-only company-bound member projection before member rows can be exposed; no owner-session forwarding or browser-origin account calls are added as a workaround.

No securely authenticated platform-to-Cadence subject-revocation hook was found in the inspected upstream runtime/account/member code. No unauthenticated callback is implemented. Proposed contract for discussion with the parent: dedicated signed platform event assertion with explicit event type, issuer, board-host audience, company, subject, event ID, issued/expiry time, short TTL and trusted JWKS; validate actor allowlist and persist replay/subject revocation state under the auth lock. A local operator subject-revoke RPC would separately require native operator process proof. New sign-in after a role change/removal needs an explicit revocation ordering contract so a delayed old assertion cannot re-open revoked authority. HTTP must enforce the same proof as daemon, and both need agent/detached/forged/concurrent adversarial tests before the guard.

This is a proposal, not an accepted upstream contract. Parent approval or a supported upstream contract is required before hook implementation. CAD-619 is not complete until member display and the trusted immediate-revocation contract are resolved and independently verified.

## Preview and checks

Stable private preview: `http://ip-172-31-1-32.tail9fcf30.ts.net:3178/`, own Vite server using `/tmp/op-codex-619-preview`. It renders the actual component with explicitly labeled mocked data and no production API proxy. HTTP and transformed component were served successfully. AWS-local Chrome timed out twice; each owned browser session was closed. Desktop/narrow/loading/error browser evidence remains pending.

Rust fmt check passed. Compiled gates await an enrolled native runner or CI; no direct build-slot bypass was used. Required focused gates: lib `cad619_`, UI typecheck, UI URL-state tests, UI build and relevant full CI. Platform read tests cover precision/shape/credential projection, safe URL construction, row/body caps and redirect refusal.
