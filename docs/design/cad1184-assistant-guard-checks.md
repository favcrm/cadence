# CAD-1184 independent guard acceptance checks

Author: independent acceptance-check author (not the implementation workers).
Source: `/tmp/crm-assistant-pm/issue-body.md` and frozen boundary in
`/tmp/crm-assistant-pm/kickoff.md`. These checks are intentionally not a
mirror/fake of the generic assistant API. They must be exercised against the
real daemon dispatcher and the board's HTTP relay after backend wiring.

## Required evidence

Each refusal case below must be made through a real installed CRM app and a
live, daemon-verified chat turn where the call is an assistant action. Capture
state before and after: no tag mutation, no permission/operation grant or
unexpected receipt, and no send/freeze/apply side effect. A positive control
must show that the corresponding valid operator decision or bounded action is
accepted; otherwise blanket refusal can falsely pass. Exercise board HTTP for
all operator-only decisions and at least the same agent/forged-caller proofs
as the daemon RPC.

| Guard under test | Adversarial request | Required observation |
| --- | --- | --- |
| Actor/decision provenance | Assistant/agent invokes permission allow-once, allow-always, deny, revoke, or block; inject `actor`, approval, authority, permission scope, or operator-proof fields into assistant invoke/decision requests. | Daemon and HTTP refuse; no authority or permission state changes. Only a real operator decision over HTTP can transition the exact pending operation. |
| Install/context/resource binding | Reuse a valid customer/action operation while forging another installation, context, customer/resource, or action id; ask an installed app to discover another app's actions. | Refused or only own installation's registered actions returned; no cross-install/customer read or write. |
| Handler allowlist | Invoke an unknown handler/action and pass a descriptor action not present in the host registry. | Fail closed before handler execution; no operation receipt claiming success and no side effect. |
| Tags-only input boundary | Submit `customer.tags.update` with `consent`, email, identity/profile, or arbitrary extra fields (both nested and top-level); alter tags for a second customer in the same request. | Schema/handler refuses; customer's tags, consent, email and all profile fields remain byte-for-byte unchanged. Valid single-customer tags request previews the actual delta and identifies exactly that customer. |
| Permission scope | Grant `allow_always` for one install/action/customer, then try another install/action/customer; exercise deny-this-request, persistent block, and revoke. | Deny leaves the requested write unapplied and creates no standing deny; block persists a deny; revoke invalidates an active grant. Mismatched scope, blocked, denied, revoked and stale grants all stop before handler execution. |
| Bound operation semantics | Create a pending tag operation, then change input, action schema/descriptor semantics, customer resource, or operation digest before decision; also repeat the exact operation idempotency key and replay decision. | Changed semantics and conflicting replay refuse. Exact replay returns the same operation/result without a second write. Stale revision refuses without state change. |
| Consequential verbs | Attempt generic dispatch of content apply, audience freeze, email send, send approval, and freeze approval (including disguised/unknown handler aliases). | Fail closed; generic dispatch does not reach these existing operator gates. Assert no content/audience/send state or provider calls changed. |

## Exact exercise procedure / integration hook

The executable test belongs in the retained test-seam path, not in a mock
handler. It should start a temp-state daemon and board (3110–3199), install
`workspace-apps/crm` through the daemon, create a CRM context and a real
customer fixture, issue a real chat message and redeem its daemon-stamped live
turn proof, then call the registered assistant action RPCs. Assert the
persisted record/permission/operation receipts through the store, and send the
same denial/decision requests through the real board HTTP route with the
fixture identity seam plus a real operator session for positive controls.

Backend owns `src/daemon.rs` module wiring and the RPC/HTTP implementation. To
wire the independently authored executable check without editing it, add
`#[cfg(all(test, feature = "test-seam"))] mod cad1184_acceptance;` alongside
`mod conversations_acceptance;` in `src/daemon.rs` and implement the stable
RPC methods using the frozen request/response contract. Do not put this test
behind a production feature or modify its assertions. The dispatch method
names and full argument schema are not enumerated in the kickoff; before
turning this matrix into a compilable daemon integration test, backend must
publish the exact daemon RPC names and the accepted assistant-invoke proof
fields so the test can call the same guard as the CLI wrapper without
inventing or mirroring an API.

## Current execution status

The executable independent test source is now
`src/daemon/cad1184_acceptance.rs`. Backend has wired its test module and
RPC names, but decision/revoke/block handlers and the HTTP relay are not
implemented yet, and the current CRM descriptor differs from the pinned
schema. The acceptance module has not been compiled or run. Execute the
focused test-seam module after those prerequisites are implemented; no guard
result is claimed by this matrix or the authored source until that run
succeeds.
