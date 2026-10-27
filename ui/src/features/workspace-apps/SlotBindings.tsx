import { useState } from "react";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import { retainedRequest } from "./requests";
import {
  bindingForSlot,
  bindingHealth,
  candidatesFor,
  plainRequirement,
  type DeclaredSlot,
} from "./bindingChoices";
import { connectionLabel } from "../../lib/connections";
import { useTranslate } from "../../lib/locale";
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
  const t = useTranslate();
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const typed = slots.filter((slot) => slot.declaration !== null);
  const untyped = slots.filter((slot) => slot.declaration === null);
  if (slots.length === 0) {
    return (
      <p className="wa-muted">
        {t("This app declares no connection slots — nothing to bind.")}
      </p>
    );
  }
  // Drafts are keyed by context and slot: an unsaved choice in one
  // brand never carries into another context's save.
  const draftKey = (slot: string) => `${contextId ?? ""}::${slot}`;
  return (
    <>
      <p className="wa-muted">
        {t("Re-saving the same connection changes nothing. Choosing a different connection closes this slot&apos;s waiting sends; new runs pin the new binding. The daemon re-checks the reviewed contract on every save.")}
      </p>
      {untyped.length > 0 && (
        <p className="wa-muted">
          {legacyNote(untyped.length, untyped.map((slot) => slot.slot).join(", "), t)}
        </p>
      )}
      {typed.map((slot) => {
        const binding = bindingForSlot(bindings, contextId, slot.slot, digest);
        const health = bindingHealth(binding, digest, connections);
        // The store refuses an update across bundle digests — a stale
        // pin rebinds through create, with a fresh request identity.
        const stale = health === "stale-bundle";
        const { offered, withheld } = candidatesFor(slot.declaration, connections);
        const key = draftKey(slot.slot);
        const selected = drafts[key] ?? binding?.config.connection_id ?? "";
        const unchanged =
          binding !== undefined && !stale && selected === binding.config.connection_id;
        return (
          <section key={slot.slot} className="wa-panel wa-stack" aria-label={`${titleFor(slot.slot)} connection`}>
            <h2>{titleFor(slot.slot)}</h2>
            <p className="wa-muted">
              {t("Save the connection for")} {contextLabel ?? t("runs without a brand context")}.{" "}
              {plainRequirement(slot, t)} {t("This configures future plans; it doesn&apos;t release anything.")}
            </p>
            {binding && health === "ok" && (
              <p className="wa-kicker">
                {t("Configured · revision")} {binding.revision} · {binding.config.account}
              </p>
            )}
            {binding && health === "stale-bundle" && (
              <p className="wa-alert">
                {t("This binding pins an older app version ({digest}). Save again to rebind the current version — runs stay gated until then.").replace("{digest}", () => shortDigest(binding.config.bundle_digest))}
              </p>
            )}
            {binding && health === "missing-connection" && (
              <p className="wa-alert">
                {t("Its connection ({id}) is gone — revoked or re-enrolled elsewhere. Choose another below.").replace("{id}", () => shortId(binding.config.connection_id))}
              </p>
            )}
            {binding && health === "not-configured" && (
              <p className="wa-alert">
                {t("This binding is {state} — save again to configure it.").replace("{state}", () => binding.state)}
              </p>
            )}
            <div className="wa-row">
              <div className="wa-context">
                <Select
                  value={selected}
                  onChange={(next) =>
                    setDrafts((current) => ({ ...current, [key]: next }))
                  }
                  options={offered.map((value) => ({
                    value: value.id,
                    label: connectionLabel(value),
                    hint: value.id,
                  }))}
                  placeholder={t("Choose {slot} connection").replace("{slot}", () => titleFor(slot.slot).toLowerCase())}
                  aria-label={t("{slot} connection").replace("{slot}", () => titleFor(slot.slot))}
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
                      delete next[key];
                      return next;
                    });
                  })
                }
              >
                {t("Save {slot} connection").replace("{slot}", () => titleFor(slot.slot).toLowerCase())}
              </Button>
            </div>
            {withheld > 0 && (
              <p className="wa-muted">
                {t(withheld === 1 ? "{count} unavailable connection is hidden —" : "{count} unavailable connections are hidden —").replace("{count}", () => String(withheld))}{" "}
                <Link href="/settings/connections">{t("see Settings → Connections")}</Link>.
              </p>
            )}
            {offered.length === 0 && (
              <p className="wa-alert">
                {t("No compatible connection is available.")}{" "}
                <Link href="/settings/connections">{t("Add one in Settings → Connections")}</Link>{t(", then come back — candidates re-check automatically.")}
              </p>
            )}
          </section>
        );
      })}
    </>
  );
}

/** Legacy untyped slots: the English sentence is split around the
 *  `cadence app set` command (a JSX element), so each piece is a whole key. */
function legacyNote(
  count: number,
  slots: string,
  t: (english: string) => string,
) {
  const lead = count === 1
    ? t("Legacy slot {slots} binds through").replace("{slots}", () => slots)
    : t("Legacy slots {slots} bind through").replace("{slots}", () => slots);
  const tail = t("— the board binds only reviewed capability slots, and an untyped slot never authorizes a connection effect.");
  return <>{lead} <code>cadence app set</code> {tail}</>;
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
