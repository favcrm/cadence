import { useState } from "react";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import { retainedRequest } from "./requests";
import {
  bindingForSlot,
  bindingHealth,
  candidatesFor,
  connectionLabel,
  plainRequirement,
  type DeclaredSlot,
} from "./bindingChoices";
import { workspaceApps, type AppBinding, type Connection } from "./workspaceApps";

/**
 * The installed-App connections panel (CAD-585): one section per
 * declared slot, in plain words, with reviewed-compatible candidates
 * only. Saves through the existing typed binding create/update calls
 * with the binding's expected revision — the daemon re-checks the
 * exact capability contract server-side, so a stale or forged choice
 * refuses there even if this filter ever drifts.
 *
 * Stale, revoked and vanished bindings stay visible with their reason
 * instead of silently reading as unbound. No secret ever passes
 * through here — only connection IDs.
 */
export function SlotBindings({
  installId,
  digest,
  contextId,
  contextLabel,
  slots,
  bindings,
  connections,
  canWrite,
  busy,
  mutate,
}: {
  installId: string;
  digest: string;
  contextId: string | null;
  contextLabel: string | null;
  slots: DeclaredSlot[];
  bindings: AppBinding[];
  connections: Connection[];
  canWrite: boolean;
  busy: boolean;
  mutate: (work: () => Promise<void>) => Promise<void>;
}) {
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  if (slots.length === 0) {
    return (
      <p className="wa-muted">
        This app declares no connection slots — nothing to bind.
      </p>
    );
  }
  return (
    <>
      {slots.map((slot) => {
        const binding = bindingForSlot(bindings, contextId, slot.slot, digest);
        const health = bindingHealth(binding, digest, connections);
        // The store refuses an update across bundle digests — a stale
        // pin rebinds through create, with a fresh request identity.
        const stale = health === "stale-bundle";
        const { offered, withheld } = candidatesFor(slot.declaration, connections);
        const selected = drafts[slot.slot] ?? binding?.config.connection_id ?? "";
        const unchanged =
          binding !== undefined && !stale && selected === binding.config.connection_id;
        return (
          <section key={slot.slot} className="wa-panel wa-stack" aria-label={`${titleFor(slot.slot)} connection`}>
            <h2>{titleFor(slot.slot)}</h2>
            <p className="wa-muted">
              Save the connection for {contextLabel ?? "runs without a brand context"}.{" "}
              {plainRequirement(slot)} This configures future plans; it
              doesn&apos;t release anything.
            </p>
            {binding && health === "ok" && (
              <p className="wa-kicker">
                Configured · revision {binding.revision} · {binding.config.account}
              </p>
            )}
            {binding && health === "stale-bundle" && (
              <p className="wa-alert">
                This binding pins an older app version ({shortDigest(binding.config.bundle_digest)}).
                Save again to rebind the current version — runs stay gated until then.
              </p>
            )}
            {binding && health === "missing-connection" && (
              <p className="wa-alert">
                Its connection ({shortId(binding.config.connection_id)}) is gone —
                revoked or re-enrolled elsewhere. Choose another below.
              </p>
            )}
            {binding && health === "not-configured" && (
              <p className="wa-alert">
                This binding is {binding.state} — save again to configure it.
              </p>
            )}
            <div className="wa-row">
              <div className="wa-context">
                <Select
                  value={selected}
                  onChange={(next) =>
                    setDrafts((current) => ({ ...current, [slot.slot]: next }))
                  }
                  options={offered.map((value) => ({
                    value: value.id,
                    label: connectionLabel(value),
                    hint: value.id,
                  }))}
                  placeholder={`Choose ${titleFor(slot.slot).toLowerCase()} connection`}
                  aria-label={`${titleFor(slot.slot)} connection`}
                  disabled={!canWrite || busy}
                  full
                />
              </div>
              <Button
                disabled={!canWrite || busy || !selected || unchanged}
                loading={busy}
                onClick={() =>
                  void mutate(async () => {
                    if (binding && !stale)
                      await workspaceApps.updateBinding(installId, binding.id, {
                        expected_revision: binding.revision,
                        connection_id: selected,
                      });
                    else
                      await workspaceApps.createBinding(installId, {
                        slot: slot.slot,
                        connection_id: selected,
                        request_id: retainedRequest(
                          JSON.stringify([installId, contextId, digest, slot.slot, selected]),
                        ),
                        ...(contextId ? { context_id: contextId } : {}),
                      });
                    setDrafts((current) => {
                      const next = { ...current };
                      delete next[slot.slot];
                      return next;
                    });
                  })
                }
              >
                Save {titleFor(slot.slot).toLowerCase()} connection
              </Button>
            </div>
            {withheld > 0 && (
              <p className="wa-muted">
                {withheld} unavailable {withheld === 1 ? "connection is" : "connections are"}{" "}
                hidden — <Link href="/settings/connections">see Settings → Connections</Link>.
              </p>
            )}
            {offered.length === 0 && (
              <p className="wa-alert">
                No compatible connection is available.{" "}
                <Link href="/settings/connections">Add one in Settings → Connections</Link>,
                then come back — candidates re-check automatically.
              </p>
            )}
          </section>
        );
      })}
    </>
  );
}

/** "publication" → "Publication", "source" → "Source" — slot ids in plain words. */
function titleFor(slot: string): string {
  const head = slot.charAt(0).toUpperCase();
  return `${head}${slot.slice(1)}`;
}

function shortDigest(digest: string): string {
  return digest.length > 18 ? `${digest.slice(0, 18)}…` : digest;
}

function shortId(id: string): string {
  return id.length > 24 ? `${id.slice(0, 24)}…` : id;
}
