import { useCallback, useContext, useEffect, useId, useRef, useState } from "react";
import { api, ApiError, type MasterPermissionRequest, type MasterPermissionRule } from "../../lib/api";
import { useTranslate } from "../../lib/locale";
import { subscribeMasterChanged } from "../../lib/masterChanged";
import Link from "../../ui/Link";
import { useWriteBlock, WriteGate } from "../auth/WriteGate";
import PermissionCard, { cardFromRequest, publishPermission, subscribePermission } from "./PermissionCard";
import { cleanPermissionInline, formatRuleCommand } from "./permissionFormat";

/**
 * Interactive `/permissions` slash card (CAD-2318): lists pending approval
 * requests and saved `permissions.yaml` rules with inline two-step revoke.
 * Native boards only — hosted boards withhold `/permissions` via `offeredCommands`.
 */
export default function PermissionsSlashCard({
  readOnly,
  permissionsHref,
}: {
  readOnly: boolean;
  permissionsHref?: string;
}) {
  const t = useTranslate();
  const gateBlock = useContext(WriteGate);
  const readOnlyBlock = useWriteBlock(readOnly);
  const blocked = readOnly || readOnlyBlock !== null || gateBlock !== null;
  const headingId = useId();
  const [data, setData] = useState<{ requests: MasterPermissionRequest[]; rules: MasterPermissionRule[] } | null>(null);
  const [loading, setLoading] = useState(true);
  const [readError, setReadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [confirmId, setConfirmId] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const alive = useRef(false);
  const request = useRef(0);

  const load = useCallback(() => {
    const seq = ++request.current;
    setLoading(true);
    api
      .permissionRules()
      .then((out) => {
        if (!alive.current || request.current !== seq) return;
        setData(out);
        setReadError(null);
      })
      .catch((error: unknown) => {
        if (!alive.current || request.current !== seq) return;
        if (error instanceof ApiError && (error.status === 401 || error.status === 403)) {
          // Clear protected data immediately when operator auth is lost.
          setData(null);
          setConfirmId(null);
          setActionError(null);
        }
        setReadError(error instanceof ApiError ? error.message : String(error));
      })
      .finally(() => {
        if (alive.current && request.current === seq) setLoading(false);
      });
  }, []);

  useEffect(() => {
    alive.current = true;
    load();
    const offLocal = subscribePermission(load);
    const offMaster = subscribeMasterChanged(load);
    return () => {
      alive.current = false;
      request.current += 1;
      offLocal();
      offMaster();
    };
  }, [load]);

  const ruleKey = (rule: MasterPermissionRule) => `${rule.agent ?? "master"}:${rule.id}`;

  const confirmRevoke = (rule: MasterPermissionRule) => {
    const key = ruleKey(rule);
    if (blocked || busyId !== null) return;
    setBusyId(key);
    setActionError(null);
    api
      .permissionRevoke(rule.id, rule.agent && rule.agent !== "master" ? rule.agent : undefined)
      .then(() => {
        if (!alive.current) return;
        setConfirmId(null);
        publishPermission();
      })
      .catch((error: unknown) => {
        if (!alive.current) return;
        if (error instanceof ApiError && (error.status === 401 || error.status === 403)) {
          // Clear protected data immediately when operator auth is lost; set readError
          // since actionError only renders inside the {data !== null} block.
          setData(null);
          setConfirmId(null);
          setActionError(null);
          setReadError(error.message);
          return;
        }
        setActionError(error instanceof ApiError ? error.message : String(error));
      })
      .finally(() => {
        if (alive.current) setBusyId(null);
      });
  };

  const pending = (data?.requests ?? []).filter((r) => r.status === "pending");
  const rules = data?.rules ?? [];

  return (
    <div className="mt-2 space-y-3 min-w-0" data-permissions-card aria-labelledby={headingId}>
      {loading && data === null && <p className="text-micro text-ink-500">{t("Reading permissions…")}</p>}

      {readError && (
        <div className="text-micro text-fail break-words" role="alert">
          {readError}{" "}
          <button type="button" className="lnk" onClick={load}>
            {t("Retry")}
          </button>
        </div>
      )}

      {pending.length > 0 && (
        <section className="space-y-2 min-w-0" aria-label={t("Needs your approval")}>
          <p className="text-micro font-medium text-warn">
            {t("Needs your approval")} ({pending.length})
          </p>
          <ul className="space-y-2">
            {pending.map((req) => (
              <li
                key={req.id}
                className="rounded border border-ink-700 bg-ink-900 px-2.5 py-2 min-w-0 space-y-1"
                data-permissions-pending={req.id}
              >
                <div className="flex flex-wrap items-center gap-1.5 min-w-0">
                  {req.requested_by && req.requested_by !== "master" ? (
                    <span className="chip bg-ink-800 text-ink-300 shrink-0" data-permission-agent={req.requested_by}>
                      <bdi>{cleanPermissionInline(req.requested_by)}</bdi>
                    </span>
                  ) : null}
                  <span className="chip bg-ink-800 text-ink-300 shrink-0">{t(req.risk)}</span>
                  <code className="num text-micro text-ink-200 break-all min-w-0">
                    <bdi>{cleanPermissionInline(req.command)}</bdi>
                  </code>
                </div>
                {req.cwd && (
                  <p className="text-micro text-ink-500 break-all">
                    <bdi>{cleanPermissionInline(req.cwd)}</bdi>
                  </p>
                )}
                <PermissionCard
                  card={cardFromRequest(req)}
                  readOnly={blocked}
                  onDone={() => setActionError(null)}
                  compact
                />
              </li>
            ))}
          </ul>
        </section>
      )}

      {data !== null && (
        <section className="space-y-2 min-w-0">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p id={headingId} className="text-micro font-medium text-ink-300">
              {t("Saved rules")} ({rules.length})
            </p>
            {permissionsHref && (
              <Link href={permissionsHref} className="lnk text-micro">
                {t("Open full permissions")}
              </Link>
            )}
          </div>

          {actionError && (
            <p className="text-micro text-fail break-words" role="alert">
              {actionError}
            </p>
          )}

          {rules.length === 0 ? (
            <p className="text-micro text-ink-500">
              {t("No saved rules yet. Choose Always or Don't ask again on a request to save one.")}
            </p>
          ) : (
            <ul className="divide-y divide-ink-700 rounded border border-ink-700 bg-ink-900">
              {rules.map((rule) => {
                const key = ruleKey(rule);
                const confirming = confirmId === key;
                const busy = busyId === key;
                const agent = rule.agent || "master";
                const scopeText = rule.scope === "exact" ? t("Exact command") : t("Argument pattern");
                return (
                  <li
                    key={key}
                    className="px-2.5 py-2 flex flex-wrap items-start justify-between gap-2 min-w-0"
                    data-permission-rule={rule.id}
                    onKeyDown={(e) => {
                      if (e.key === "Escape" && confirming) {
                        e.preventDefault();
                        setConfirmId(null);
                      }
                    }}
                  >
                    <div className="min-w-0 flex-1 space-y-1">
                      <div className="flex flex-wrap items-center gap-1.5 text-micro">
                        <span
                          className={`chip chip-sans ${rule.effect === "allow" ? "bg-ok/15 text-ok" : "bg-fail/15 text-fail"}`}
                          data-effect={rule.effect}
                        >
                          {rule.effect === "allow" ? t("Allow") : t("Deny")}
                        </span>
                        <span className="chip bg-ink-800 text-ink-300" data-rule-agent={agent}>
                          <bdi>{cleanPermissionInline(agent)}</bdi>
                        </span>
                        <span className="text-ink-400">{scopeText}</span>
                      </div>
                      <code className="block num text-micro text-ink-200 break-all">
                        <bdi>{formatRuleCommand(rule)}</bdi>
                      </code>
                      <p className="text-micro text-ink-500 break-all">
                        {rule.cwd ? <bdi>{cleanPermissionInline(rule.cwd)}</bdi> : t("Not recorded")}
                      </p>
                    </div>
                    <div className="flex flex-wrap items-center gap-1.5 shrink-0">
                      {confirming ? (
                        <>
                          <button
                            type="button"
                            className="btn btn-sm"
                            disabled={busy}
                            onClick={() => setConfirmId(null)}
                          >
                            {t("Cancel")}
                          </button>
                          <button
                            type="button"
                            className="btn btn-sm border-fail text-fail"
                            disabled={blocked || busy}
                            data-permission-confirm-revoke={rule.id}
                            onClick={() => confirmRevoke(rule)}
                          >
                            {busy ? t("Revoking…") : t("Confirm revoke")}
                          </button>
                        </>
                      ) : (
                        <button
                          type="button"
                          className="btn btn-sm"
                          disabled={blocked || busyId !== null}
                          data-permission-revoke={rule.id}
                          onClick={() => {
                            setActionError(null);
                            setConfirmId(key);
                          }}
                        >
                          {t("Revoke")}
                        </button>
                      )}
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </section>
      )}
    </div>
  );
}
