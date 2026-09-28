# Trusted provider deployment metadata

The hosted image composition may bake `/etc/cadence/provider-deployments.json`:

```json
{
  "schema": 1,
  "providers": [
    {
      "provider": "agenticos",
      "origin": "http://api.internal",
      "manifest_pin": "agenticos-manifest@1/publish_post@2"
    },
    {
      "provider": "agenticos_external",
      "origin": "https://api-v2.agenticos.hk",
      "manifest_pin": "agenticos-external-provider-tools@1",
      "image_hosts": ["cdn.example.invalid"]
    }
  ]
}
```

The external entry is illustrative. Replace `cdn.example.invalid` only with
the exact lowercase public DNS hostname independently verified for the image
provider's returned URLs; it is not a live CDN assertion. Source reads may
omit `image_hosts`. An absent or nonmatching image host keeps image acquisition
closed, even when the manifest pin matches. At most four distinct hosts are
allowed; each is a hostname, not a URL, wildcard or IP address. The image
fetch refuses redirects and proxies, resolves once, and rejects private or
non-public addresses before retaining bounded bytes.

This is an image-owned assertion about an independently reviewed deployed
contract. It is not remote discovery, an account binding, consent, a credential,
or verification of a hosted company. The image owner must update the assertion
when the deployment changes. This repository change does not install that file,
change the hosted image, or prove a live upstream deployment.

The fixed file and every ancestor must be root-owned and not writable by group
or others. The reader pins each directory with `openat`, refuses symlinks and
nonregular files, and reads at most 16 KiB. Missing optional metadata supplies no
assertion. Unsafe or malformed present metadata refuses startup with a sanitized
message. Duplicate and unknown JSON fields, null entries and unsupported schema
versions are rejected. No environment variable, CLI argument, PM configuration,
app input, or board route selects the file or supplies a pin.

Only a hosted attachment whose resolved origin exactly matches the provider
entry consumes its pin. An origin override cannot borrow another origin's pin.
Absent and mismatched pins retain Cadence's Send gate. A matched reviewed
AgenticOS pin classifies `publish_post` as a draft approval handoff; AgenticOS
still owns approval. The existing runtime door sends the exact content hash and
durable idempotency key to `/v1/runtime/connectors/publish`. No app-effect guard,
grant, or outward release approval is bypassed.

The separate `agenticos_external` pin identifies the exact deployed external
provider-tools contract for priced Social Content reads and drafts. The app
cannot supply or change its origin, manifest pin or image host allowlist.
An exact pin is necessary but does not grant a device credential, approve a
quote, or authorize a paid call.

Embedding code may supply typed `ServeOptions::provider_deployments` instead of
the fixed file. This is a programmatic trusted-composition seam, never serialized
from caller inputs. Deterministic fixtures use that seam; loader unit tests use
an internal test owner parameter. Production always requires UID 0. These tests
are not evidence of a root-owned image installation or live hosted operation.
