# Fixture image for the `native-oci-agent/1` protected runtime.
#
# The base must be an explicitly digest-pinned image (no floating tag): the
# build script refuses a reference without `@sha256:<64 lowercase hex>`. The
# staged rootfs contains only the pinned Node runtime, the pinned Pi graph
# entry, the observer extension, the reviewed NAPI seal addon and the pinned
# material graph; the profile generator computes their real digests from this
# tree.
ARG BASE
FROM ${BASE} AS runtime_base

# Reuse the pinned filesystem, not its credential-bearing environment,
# command, entrypoint, volumes or other application image configuration.
FROM scratch
COPY --from=runtime_base / /

# Everything is installed root-owned and not group/other writable.
COPY rootfs/ /
RUN chmod 0755 /opt/cadence /opt/cadence/node /opt/cadence/node/bin \
 && chmod 0755 /opt/cadence/node/bin/node \
 && chmod 0555 /opt/cadence/node/observer.mjs /opt/cadence/node/pi.mjs \
              /opt/cadence/node/cadence_seal.node \
 && chmod -R a-w /opt/cadence/node/node_modules /opt/cadence/material \
 && find /opt/cadence/node/node_modules -type d -exec chmod 0555 {} + \
 && find /opt/cadence/node/node_modules -type f -exec chmod 0444 {} + \
 && find /opt/cadence/material -type d -exec chmod 0555 {} + \
 && find /opt/cadence/material -type f -exec chmod 0444 {} +

# No command is baked in: the owner launches the exact pinned argv and the
# container's user/namespace/seccomp limits come from the root profile.
USER 21001:21001
WORKDIR /home/agent
