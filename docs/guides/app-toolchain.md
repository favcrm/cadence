# Versioned app toolchain

Cadence publishes a platform-neutral `cadence-app-toolchain-<tag>.tar.gz` asset and `.sha256` sidecar on its existing versioned GitHub Releases. It is optional starter tooling for app-owned React/Vite projects and ships the app-side screen-bridge receiver (`app-kit/`). It is not an SDK, an authority or an app runtime. It does not change SOC-3 app adoption or SOC-1/SOC-5 holds.

For the end-to-end starter, see [App development](app-development.md) and
[`app-toolchain/template/`](../../app-toolchain/template/README.md).

## Download and verify

Choose a published Cadence release tag and download both matching assets. For example:

```sh
tag=v<release-version>
asset="cadence-app-toolchain-${tag}.tar.gz"
gh release download -R favcrm/cadence "$tag" -p "$asset" -p "$asset.sha256"
sha256sum -c "$asset.sha256"
```

Before consuming the archive, verify its GitHub build-provenance attestation against the exact release tag and Cadence workflow, as described in [INSTALL-AGENT.md](../INSTALL-AGENT.md). The checksum detects byte changes relative to its sidecar; it does not replace provenance verification. Releases are immutable once published.

Extract into a new app-owned project directory. The archive's `toolchain.json` records schema, release tag, source commit, package manager, source-derived pins and the receiver's version and per-file sha256 (`app_kit`). `bun.lock` freezes the dependency graph. The current archive is tested with Node 22 and Bun 1.4.4; use the exact package-manager version recorded in `toolchain.json` and run `bun install --frozen-lockfile`. Do not regenerate the lock during routine installation. See the packaged [`app-toolchain/README.md`](../../app-toolchain/README.md) for the minimal consumer flow.

## Screen bridge receiver

`app-kit/` is the app side of the screen bridge, vendor-copied from the
Cadence UI source and pinned in `toolchain.json` under `app_kit`. Import
`app-kit/src/index.mjs` from the unpacked archive (or vendor the folder into
the app) instead of writing a private `postMessage` receiver. The receiver is
transport and lifecycle only: the app still owns its projection, its closed
verb set and its payload validation. Do not edit the packaged files; a new
Cadence release ships the next version.

## Matching frame kit

The archive's `frame-kit/frame.html`, `kit.css`, and `metadata.json` come from `cadence app frame-kit` in the matching built CLI. Use that CLI version to generate a fresh frame for the app's actual validated bundle and actual parent origin:

```sh
cadence app frame-kit --out /tmp/my-app-frame \
  --bundle ./path/to/app-bundle --parent-origin http://localhost:5173
```

The parent origin must be the actual frame parent; the kit accepts HTTPS or an
HTTP `localhost`/`*.localhost` origin with a port, not a loopback IP literal.

The packaged files are canonical host output, not a copied editable renderer. The default offline frame is an empty reference, not an app-mounted harness. This toolchain alone does not establish Social, native API, or frame-harness integration.

## Shared UI design

The separate `@cadence/screen-ui` package owns reusable visual primitives and
styles, not bridge transport or authority. Its
[`DESIGN.md`](../../ui/screen-ui/DESIGN.md) defines the compact app design
guidance, composition ownership, accessibility and host-versus-app drawer
boundary. The screen-ui packaging script includes that file and hashes it in
both source provenance and the archive inventory. Consumers of older pinned
archives should consult the source guide until they adopt a release containing
it; adding the guide does not silently upgrade an installed package.
