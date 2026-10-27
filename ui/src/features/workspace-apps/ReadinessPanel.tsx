import Button from "../../ui/Button";
import { useTranslate } from "../../lib/locale";
import { readinessAction, readinessFor } from "./bindingChoices";
import type {
  AppBinding,
  BindingChange,
  Connection,
  Installation,
} from "./workspaceApps";

/**
 * The installed-App Ready-to-run view (CAD-796, CAD-1119). Installing the
 * app is its approval, so this lists only real blockers: a slot with no
 * current binding, an unhealthy or unregistered connection, or a
 * connection whose slot contract widened. The last one carries the
 * change and one inline confirm, which re-binds the same connection
 * through the typed update call — the daemon re-checks the contract.
 * Every value comes from the installed bundle, saved bindings and listed
 * connections the board already fetched, so it can never invent
 * authority.
 */
export function ReadinessPanel({
  installation,
  bindings,
  connections,
  contextId,
  canWrite = false,
  busy = false,
  onConfirm,
}: {
  installation: Installation;
  bindings: AppBinding[];
  connections: Connection[];
  contextId: string | null;
  canWrite?: boolean;
  busy?: boolean;
  onConfirm?: (binding: AppBinding) => void;
}) {
  const t = useTranslate();
  const rows = readinessFor(installation, bindings, connections, contextId, t);
  if (rows.length === 0) return null;
  const blockers = rows.filter((row) => !row.ready);
  return (
    <section className="wa-panel wa-stack" aria-label={t("Ready to run")}>
      <h2>{t("Ready to run")}</h2>
      <p className="wa-kicker" data-tone={blockers.length ? "warn" : "ok"}>
        {blockers.length
          ? t("Something still needs you before this app can run.")
          : t("Every connection slot is bound and healthy.")}
      </p>
      {blockers.map((row) => (
        // The row keeps the one-line title and status; the next action, the
        // change and the confirm sit in a full-width block under it, so the
        // confirm never leaves the panel at any width.
        <div key={row.slot} className="wa-blocker" aria-label={`${row.slot} readiness`}>
          <div className="wa-step">
            <span className="wa-blocker-title">
              {titleFor(row.slot)} · {row.requirement}
            </span>
            <span className="wa-status" data-tone="warn">
              {t("Needs you")}
            </span>
          </div>
          <div className="wa-blocker-detail">
            <p className="wa-muted">{readinessAction(row, t)}</p>
            {row.confirm && row.binding && (
              <>
                <ul className="wa-diff" aria-label={t("{slot} connection change").replace("{slot}", () => row.slot)}>
                  {row.confirm.map((change) => (
                    <li key={change.field}>{describe(change)}</li>
                  ))}
                </ul>
                {onConfirm && (
                  <div className="wa-row">
                    <Button
                      size="sm"
                      disabled={!canWrite || busy}
                      loading={busy}
                      onClick={() => row.binding && onConfirm(row.binding)}
                    >
                      {t("Confirm {slot} change").replace("{slot}", () => row.slot)}
                    </Button>
                  </div>
                )}
              </>
            )}
          </div>
        </div>
      ))}
    </section>
  );
}

/** "publication" → "Publication" — slot ids in plain words. */
function titleFor(slot: string): string {
  const head = slot.charAt(0).toUpperCase();
  return `${head}${slot.slice(1)}`;
}

/** One changed receipt field, before and after, in one line. */
function describe(change: BindingChange): string {
  return `${change.field}: ${show(change.from)} → ${show(change.to)}`;
}

function show(value: unknown): string {
  if (value === undefined || value === null) return "none";
  if (Array.isArray(value)) return value.length ? value.map(String).join(", ") : "none";
  return typeof value === "string" ? value : JSON.stringify(value);
}
