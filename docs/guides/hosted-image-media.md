# Hosted image media composition (CAD-868)

This is an image-owner compatibility contract, not a caller option, credential
exchange or production activation. AOS-119 owns the later runtime image update,
schema migration gate and pilot. No paid provider calls or fleet changes are
part of this repair.

## Trusted admission

The fixed `/etc/cadence/provider-deployments.json` loader retains its root-owner,
non-writable, no-symlink and bounded-file checks. A trusted embedding may instead
supply `ServeOptions.provider_deployments` from `DeploymentMetadata::parse`.
Neither PM/app config, RPC, CLI nor an environment variable can select hosted
media mode. The optional provider transport assertion must be a string with this
exact tuple:

```json
{
  "schema": 1,
  "providers": [{
    "provider": "agenticos_external",
    "origin": "http://api.internal",
    "manifest_pin": "agenticos-external-provider-tools@3",
    "transport": "hosted-media-lease@1"
  }]
}
```

Unknown/null/non-string transport assertions, wrong provider/pin/origin, aliases,
ports, path/query/userinfo and normalization variants refuse. An absent
transport is an ordinary pin, not hosted permission. The existing `agenticos`
publisher metadata entry remains separate and can coexist in the same file;
a publisher pin alone never admits media. Validated metadata alone registers
hosted media without `CADENCE_AGENTICOS_EXTERNAL_URL`. Any present external URL
(including empty) conflicts and refuses; conflicting pre-registration also
refuses rather than replacing its transport. Compatible hosted reattachment is
idempotent. Without a hosted assertion, existing trusted pre-registration is
preserved.

External construction retains its HTTPS-or-local-test origin policy, bearer
requirements, descriptor and registration digest format. It cannot construct
internal HTTP mode, even with the reviewed pin or an account named `hosted`.
Hosted construction requires a private typed admission from validated metadata;
there is no public origin override or new release test seam.

## Capability and custody boundary

Hosted `agenticos_external` exposes only builtin account `hosted`, tool/action
`generate_image`, capability `media.generate@1`, effect `draft`, scope label
`provider.draft` and `PreviewOnly` semantics. This label is Cadence review
vocabulary, not a credential to forward. No source read, publisher sender,
legacy execution or enrollment shape is added.

Only the run-bound quote/execute broker uses the adapter's default-false
conditional credentialless hook. Empty bytes require the currently registered
adapter, validated descriptor builtin account, actual builtin connection ID and
kind, and matching workspace/registration/descriptor/pin receipts. This account
is **not** added to the global builtin account table. Legacy grants, defaults
and credential loading remain enrollment-required. An enrolled external account
merely named `hosted` is a separate connection: it cannot silently become the
builtin or supply a fake bearer. Hosted quote and execution independently refuse
nonempty bytes, non-hosted accounts and non-builtin kinds before price/network
traffic.

Discovery uses registered descriptor builtin accounts. Binding receipts are
recomputed from the current connection and sink registration. Hosted and
external registration identities differ; removing the adapter or changing its
mode/origin/pin/descriptor stales old bindings and run receipts. No synthetic
custody record or binding permission is persisted by discovery.

Price GET, image POST, job GET and artifact GET omit Authorization entirely in
hosted mode; external mode still sends its enrolled bearer. State bridge remains
the only lease owner; the Worker derives company/tenant/instance from provisioning
and the current lease, never from a Cadence request identity. This does not add a
heartbeat, `hosted.lease` or internet enablement.

## Unchanged runtime semantics and limits

CAD-816 normalized input, frozen run/turn/binding authority, execution-time
re-quote, one POST per call, exact caller idempotency key, at least ten-second
polling, bounded polling deadline, charge/replay receipts, digest/MIME verification
and honest uncertain outcomes remain unchanged. Existing operator and assigned
managed-endpoint gates still apply. Hosted mode does not auto-approve bindings,
runs, publication or any live operation.

The upstream can retain up to 10 MiB, but Cadence intentionally accepts at most
2 MiB and a fully decoded square image of at most 2048 pixels per side. Successful
upstream generation can therefore exceed custody limits and fail to produce a
retained Cadence asset. This repair does not re-encode images or broaden those
limits; a charged or uncertain upstream outcome must not be represented as a
successful retained asset or blindly resubmitted under a fresh key.

## Verification ownership

Stage A test-only revision `911d729d7e435325a39de42f34e2e5fa2973f203`
compiled in authorized PR CI run `36735419123`. Its positive metadata and
metadata-only attach tests failed semantically with trusted metadata refused;
this was not a compile failure. The repair adds adversarial metadata, attachment,
mode/header and actual discovery/binding/broker consumer tests using synthetic
local fixtures. Existing CAD-632 native assigned-turn/one-result/quote coverage
and CAD-816 image custody/release coverage are reused, not newly claimed.

The external writer has no admitted native build slot and does not run Cargo
locally. Parent-owned exact-head CI and two independent Standards and Spec/security
reviews remain necessary. This trust-boundary change requires native exact-head
operator approval before merge queue entry. A draft PR is a durability ledger,
not approval or release evidence.
