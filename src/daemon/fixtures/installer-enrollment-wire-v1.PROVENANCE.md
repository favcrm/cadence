# CAD-1061 — installer-enrollment wire fixture provenance ledger

This ledger records the *exact* public test material, its merged-owner source
and the fixed gitleaks exception scope for
[`installer-enrollment-wire-v1.json`](./installer-enrollment-wire-v1.json).

## What the fixture is

Four **public synthetic** interoperability vectors (I1–I4) for the
installer-enrollment signed-wire contract
(`aos121-installer-enrollment-signed-wire-contract` v1, contract SHA256
`07d8a6760a2d809d5442a1a2cd330e044b4c03cadf61232060e53859c6c150d4`). They carry
the published RFC8032 §7.1 test-1 Ed25519 key (`seedHex
9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60`, public
`d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a`) — *test-only
material, never provisioned*. The vectors exercise the codec: I1 baseline, I2
`imageLane` presence, I3 max JS-safe numbers + u64-string starttimes, I4
keyVersion lookup refusal.

## Merged-owner sync

- Platform PR314 merged to staging at commit
  `29798c9dfd66db673d08757c22f1e8bee37302e1` (CI run `37079891870` SUCCESS).
- This file is **byte-identical** to the merged owner
  `apps/api/src/runtime/fixtures/installer-enrollment-wire-v1.json` (`cmp` exit
  0). Fixture SHA256 `79f324cb4fad57a818fc8ea5dd5977bd885b3f945d68517658ed9e8750ad9b3a`.
- Per-vector header/payload/domain-message/signature/public-key were recomputed
  and matched; the Rust `ring` Ed25519 verifier consumes the same bytes.
  Interoperability is proven against the *merged* owner, not a draft.

## Gitleaks exception scope (fixed, exact)

The committed root `.gitleaksignore` suppresses **only** the `jwt` rule's four
findings on this fixture's `envelope` strings — the `H.P.S` base64url compact
receipts match the JWT shape but are the public synthetic vectors above. The
exception is pinned to the exact commit-scoped fingerprints produced when the
fixture was introduced (`8c49afa3f0d0d6f0aba5f3f928155ec237e79a22`), at fixed
lines `77/152/226/300`. It is *not* a path-wide, rule-wide or wildcard
allowance; the scanner, workflow and vendored `src/secret/gitleaks.toml` rules
are unchanged. The fixture, its hash, the parser and the scanner must not be
edited to evade the check.

## In-process scanner baseline (test-only)

The repo's own `secret::tests::no_new_findings_in_src` guard runs cadence's
in-process gitleaks-rule scanner over `src/` and asserts no credential-shaped
string beyond the pinned `KNOWN_FIXTURES`. The fixture's four `envelope`
strings trip the same `jwt` rule there, so four per-finding `(file, rule,
fingerprint)` tuples are pinned in `src/secret/tests.rs`. The fingerprint is
`SHA256(secret)[0..8]` hex, recomputed independently: I1 `641e6f5a108d38af`,
I2 `a15f212362eab9f1`, I3 `1aafb7cb743005dd`, I4 `19280393965df1b4`. This is a
*test-only baseline* for the same public vectors — a per-finding pin, not a
path or rule allowlist; the scan and its assertion are unchanged.

## Finite point/scalar encoding gates

The verifier mirrors the platform owner's `point()`/`strictSignature()`
policy with finite byte-encoding checks before `ring` verify: a trusted
public key and the signature's `R` must be a canonical, non-small-order point
(`y < p = 2^255-19`, `y ∉ {0, 1, p-1, 2707…027, 5518…927}`, sign bit cleared
on a copy) and `S < L`. These gates exist because raw `ring`'s ref10 verifier
*accepts* the identity-key forgery (`R = identity, S = 0` under `A =
identity` collapses `s·B = R + h·A` to `0 = 0`); the consumer refuses what
raw ring would admit. No curve engine and no new dependency — byte checks
only.

## Security posture

- The crypto here is **diagnostic/format evidence only** — verify-only. It
  cannot open a production factory, grant a `GuestCtx`, consume, enroll,
  retire, release or start a process.
- Production trusted-key/measurement/current-state factories remain closed:
  `production_trust_set` returns `Err`, `PRODUCTION_TRUST_KEYS` is empty.
- Runtime eligibility stays `false`; operational custody remains `UNKNOWN`.
