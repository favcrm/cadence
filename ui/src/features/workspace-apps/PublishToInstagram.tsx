import { useCallback, useEffect, useId, useRef, useState } from "react";
import { HugeiconsIcon } from "@hugeicons/react";
import { InformationCircleIcon } from "@hugeicons/core-free-icons";
import Button from "../../ui/Button";
import { navigate } from "../../lib/useLocation";
import { useTranslate } from "../../lib/locale";
import { retainedRequest, completeRequest } from "./requests";
import { accountLabel, connectReturnTo, destinationBinding, destinationBindings, sendSlot, bindingToUse, type PublishToolkit } from "./socialConnect";
import { workspaceApps, type AppBinding, type Installation, type PublishDestination } from "./workspaceApps";

type Listed = { unavailable: boolean; destinations: PublishDestination[] } | "loading";

const CONNECT_UNAVAILABLE = "Connecting accounts isn’t available for this workspace yet.";
const CONNECT_FAILED = "Couldn’t open the connect page. Try again.";
const USE_FAILED = "Couldn’t use that account for publishing. Reload and try again — nothing was changed.";
const STOP_FAILED = "Couldn’t stop publishing to that account. Reload and try again — nothing was changed.";
const TOOLKIT_LABEL: Record<string, string> = { instagram: "Instagram", facebook: "Facebook Page" };

function PublishingHelp({ text, label }: { text: string; label: string }) {
  const id = useId();
  const trigger = useRef<HTMLButtonElement>(null);
  const tooltip = useRef<HTMLDivElement>(null);
  const hideTimer = useRef<number | undefined>(undefined);
  const [open, setOpen] = useState(false);

  function hide() {
    window.clearTimeout(hideTimer.current);
    tooltip.current?.hidePopover();
  }

  function show() {
    window.clearTimeout(hideTimer.current);
    const button = trigger.current;
    const popup = tooltip.current;
    if (!button || !popup) return;
    popup.showPopover();
    const anchor = button.getBoundingClientRect();
    const bounds = popup.getBoundingClientRect();
    popup.style.left = `${Math.max(12, Math.min(anchor.left, window.innerWidth - bounds.width - 12))}px`;
    popup.style.top = `${anchor.bottom + 8 + bounds.height <= window.innerHeight - 12
      ? anchor.bottom + 8
      : Math.max(12, anchor.top - bounds.height - 8)}px`;
  }

  function leave() {
    window.clearTimeout(hideTimer.current);
    hideTimer.current = window.setTimeout(() => {
      if (!trigger.current?.matches(":focus") && !tooltip.current?.matches(":hover")) hide();
    }, 120);
  }

  useEffect(() => () => window.clearTimeout(hideTimer.current), []);
  useEffect(() => {
    if (!open) return;
    const dismiss = (event: globalThis.KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      hide();
    };
    document.addEventListener("keydown", dismiss, true);
    window.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide);
    return () => {
      document.removeEventListener("keydown", dismiss, true);
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("resize", hide);
    };
  }, [open]);

  return <span className="wa-publish-help-wrap">
    <button ref={trigger} type="button" className="wa-publish-help" aria-label={label} aria-describedby={id} aria-expanded={open}
      onFocus={show} onBlur={hide} onClick={show}
      onPointerEnter={event => { if (event.pointerType !== "touch") show(); }} onPointerLeave={leave}>
      <HugeiconsIcon icon={InformationCircleIcon} size={16} strokeWidth={1.7} aria-hidden="true" />
    </button>
    <div ref={tooltip} id={id} role="tooltip" popover="auto" className="wa-publish-help-tooltip"
      onToggle={event => setOpen(event.newState === "open")}
      onPointerEnter={() => window.clearTimeout(hideTimer.current)} onPointerLeave={leave}>{text}</div>
  </span>;
}

/**
 * CAD-1290 — Settings > Publish to Instagram. The owner connects Instagram on
 * AgenticOS (the host composes the link; local boards keep the local
 * connections page), sees the company's connected accounts, and taps "Use for
 * publishing" to point this install's publication binding at one of them for
 * the active context. The daemon re-checks the account against the company's
 * own list, and the binding revision, so a stale or forged choice refuses there.
 * CAD-1939: Facebook Pages too, and each account used is its own binding:
 * "Use" adds one, "Stop" retires one while another stays.
 */
export function PublishToInstagram({ installation, bindings, contextId, canWrite, busy, refreshKey, mutate }: {
  installation: Installation; bindings: AppBinding[]; contextId: string | null; canWrite: boolean; busy: boolean;
  /** Changes when the board came back from AgenticOS: read the accounts again. */
  refreshKey: number;
  mutate: (work: () => Promise<void>) => Promise<void>;
}) {
  const t = useTranslate();
  const installId = installation.install_id;
  const slot = sendSlot(installation);
  const used = slot ? destinationBindings(bindings, slot, contextId, installation.digest) : [];
  const [listed, setListed] = useState<Listed>("loading");
  const [note, setNote] = useState<string | null>(null);
  const load = useCallback((signal?: AbortSignal) => {
    setListed("loading");
    workspaceApps.destinations(installId, signal)
      .then(value => { if (!signal?.aborted) setListed(value); })
      .catch(() => { if (!signal?.aborted) setListed({ unavailable: true, destinations: [] }); });
  }, [installId]);
  useEffect(() => {
    const controller = new AbortController();
    load(controller.signal);
    return () => controller.abort();
  }, [load, refreshKey]);
  // Just back from AgenticOS: the new connection can take a moment to show, so
  // read again a few times until an account appears (no restart needed).
  useEffect(() => {
    if (refreshKey === 0) return;
    let left = 5;
    const timer = window.setInterval(() => {
      if (left-- <= 0) { window.clearInterval(timer); return; }
      workspaceApps.destinations(installId).then(value => {
        if (value.destinations.length) { setListed(value); window.clearInterval(timer); }
      }).catch(() => {});
    }, 3000);
    return () => window.clearInterval(timer);
  }, [installId, refreshKey]);
  if (!slot) return null;
  const connect = async (toolkit: PublishToolkit) => {
    setNote(null);
    try {
      // CAD-1438: this panel only mounts on the canonical `/apps/<key>` route
      // (uniqueness proven before mount), so the install's own key is its path.
      const reply = await workspaceApps.connectLink(installId, connectReturnTo(window.location.origin, installation.name, installId), undefined, toolkit);
      if (reply.hosted && reply.url) window.location.assign(reply.url);
      else if (reply.hosted) setNote(CONNECT_UNAVAILABLE);
      else navigate("/settings/connections");
    } catch { setNote(CONNECT_FAILED); }
  };
  const rows = listed === "loading" ? [] : listed.destinations;
  const expired = listed === "loading" || listed.unavailable ? [] : used.flatMap(value => {
    const publish = value.config.publish;
    return publish && !rows.some(row => row.destination_id === publish.destination_id && row.toolkit === publish.toolkit) ? [publish] : [];
  });
  const current = used.length > 0;
  return (
    <section className="wa-panel wa-stack wa-publish-settings" aria-label={t("Publish to Instagram and Facebook")} data-publish-instagram>
      <div className="wa-publish-heading">
        <h2>{t("Publish to Instagram and Facebook")}</h2>
        <PublishingHelp label={t("About publishing accounts")}
          text={t("Choose the Instagram accounts and Facebook Pages this app publishes to in the current context. Connect or reconnect an account to update the list.")} />
      </div>
      {listed === "loading" && <p className="wa-muted" role="status">{t("Checking accounts…")}</p>}
      {listed !== "loading" && listed.unavailable && (
        <p className="wa-alert" role="alert">{t("Couldn’t check your accounts right now.")} <Button size="sm" onClick={() => load()}>{t("Try again")}</Button></p>
      )}
      {listed !== "loading" && !listed.unavailable && rows.length === 0 && expired.length === 0 && <p className="wa-muted">{t("Not connected")}</p>}
      {expired.map(publish => (
        <p key={`${publish.toolkit}:${publish.destination_id}`} className="wa-alert">{t("Connection expired — {account} can’t publish until you reconnect.").replace("{account}", () => accountLabel(publish.destination_label))}</p>
      ))}
      {!current && rows.length > 0 && <p className="wa-muted">{t("Choosing an account enables publishing. You can’t restore preview mode from these settings.")}</p>}
      {!canWrite && <p className="wa-muted">{t("Read-only — publishing settings can’t be changed here.")}</p>}
      {busy && <p className="wa-muted" role="status">{t("Updating app settings…")}</p>}
      <ul className="wa-publish-accounts">
        {rows.map(row => {
          const using = destinationBinding(bindings, slot, contextId, installation.digest, row.destination_id, row.toolkit);
          return (
            <li key={`${row.toolkit}:${row.destination_id}`} className="wa-publish-account" data-destination={row.destination_id} data-toolkit={row.toolkit}>
              <strong>{accountLabel(row.label || row.destination_id)}</strong>
              <span className="wa-muted">{t(TOOLKIT_LABEL[row.toolkit] ?? row.toolkit)}</span>
              {using ? <>
                <span className="wa-kicker">{t("Publishing here")}</span>
                {used.length > 1 && (
                  <Button className="wa-publish-account-action" size="sm" disabled={!canWrite || busy}
                    onClick={() => void mutate(async () => {
                      try {
                        await workspaceApps.revokeBinding(installId, using.id, using.revision);
                      } catch {
                        throw new Error(STOP_FAILED);
                      }
                    })}>{t("Stop publishing here")}</Button>
                )}
              </> : (
                <Button className="wa-publish-account-action" size="sm" variant="primary" disabled={!canWrite || busy}
                  onClick={() => void mutate(async () => {
                    const target = bindingToUse(bindings, slot, contextId, installation.digest, row.destination_id, row.toolkit);
                    const key = JSON.stringify([installId, "use-destination", contextId, slot, row.toolkit, row.destination_id, target?.revision ?? 0]);
                    try {
                      await workspaceApps.useDestination(installId, {
                        destination_id: row.destination_id,
                        toolkit: row.toolkit,
                        ...(contextId ? { context_id: contextId } : {}),
                        ...(target ? { expected_revision: target.revision } : { request_id: retainedRequest(key) }),
                      });
                      completeRequest(key);
                    } catch {
                      throw new Error(USE_FAILED);
                    }
                  })}>{t("Use {account} for publishing").replace("{account}", () => accountLabel(row.label || row.destination_id))}</Button>
              )}
            </li>
          );
        })}
      </ul>
      {note && <p className="wa-alert" role="alert">{t(note)}</p>}
      {canWrite && listed !== "loading" && (
        <div className="wa-row">
          <Button onClick={() => void connect("instagram")} disabled={busy}>{expired.length ? t("Reconnect") : rows.length ? t("Connect another account") : t("Connect Instagram")}</Button>
          <Button onClick={() => void connect("facebook")} disabled={busy}>{t("Connect a Facebook Page")}</Button>
        </div>
      )}
    </section>
  );
}
