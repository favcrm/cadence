import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { homeDecisions } from "../decisions/useDecisions";
import {
  decideAccessRequest,
  durationLabel,
  parseAccessRequestPayload,
  scopeShortLabel,
  type AccessCardModel,
  type AccessRequestDuration,
} from "../home/accessRequest";
import type { HumanPerson } from "../threads/humanThreadsClient";
import { wiki, type WikiAccessGrant, type WikiAccessOverview } from "./api";
import { Note } from "./shared";

/**
 * AOS-816 (CAD-2885): Owner-only inline Wiki Access panel.
 * Displays inherited ACL policy, inline pending requests (`Approve`/`Decline`),
 * active explicit per-path grants (`Revoke`), and an inline grant form.
 * Personal `users/...` folders and `agents/master/permissions.yaml` remain non-grantable.
 */
export default function WikiAccessPanel({
  path,
  open,
  onClose,
  people,
  readOnly,
  onToast,
}: {
  path: string;
  open: boolean;
  onClose: () => void;
  people: readonly HumanPerson[];
  readOnly: boolean;
  onToast: (kind: "ok" | "err" | "warn", text: string) => void;
}) {
  const t = useTranslate();
  const [overview, setOverview] = useState<WikiAccessOverview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [subjectId, setSubjectId] = useState("");
  const [mode, setMode] = useState<"read" | "write">("read");
  const [duration, setDuration] = useState<AccessRequestDuration>("ongoing");
  const [busyKey, setBusyKey] = useState<string | null>(null);

  const memberOptions = useMemo(() => {
    const out: { subject: string; label: string }[] = [];
    const seen = new Set<string>();
    for (const p of people) {
      if (!p.subject || seen.has(p.subject)) continue;
      seen.add(p.subject);
      out.push({ subject: p.subject, label: p.name || p.id });
    }
    return out;
  }, [people]);

  useEffect(() => {
    if (!subjectId && memberOptions.length > 0) {
      setSubjectId(memberOptions[0]!.subject);
    }
  }, [memberOptions, subjectId]);

  const load = useCallback(async () => {
    if (!open || !path) return;
    setLoading(true);
    setError(null);
    try {
      const next = await wiki.access(path);
      setOverview(next);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [open, path]);

  useEffect(() => {
    void load();
  }, [load]);

  if (!open || !path) return null;

  const personLabel = (id: string) =>
    memberOptions.find((m) => m.subject === id)?.label ?? id;

  const pendingCards: AccessCardModel[] = (overview?.pending_requests ?? [])
    .map(parseAccessRequestPayload)
    .filter((card): card is AccessCardModel => card !== null);

  const addGrant = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!subjectId || readOnly || busyKey !== null) return;
    setBusyKey("add");
    try {
      await wiki.setGrant({ path, subject_id: subjectId, mode, duration });
      onToast("ok", t("Updated access grant."));
      await load();
    } catch (err) {
      onToast("err", err instanceof Error ? err.message : String(err));
    } finally {
      setBusyKey(null);
    }
  };

  const revokeGrant = async (grant: WikiAccessGrant) => {
    if (readOnly || busyKey !== null || grant.inherited) return;
    setBusyKey(`revoke:${grant.subject_id}`);
    try {
      await wiki.revokeGrant({ path: grant.path, subject_id: grant.subject_id });
      onToast("ok", t("Revoked access grant."));
      await load();
    } catch (err) {
      onToast("err", err instanceof Error ? err.message : String(err));
    } finally {
      setBusyKey(null);
    }
  };

  const decideInline = async (card: AccessCardModel, verb: "approve" | "decline") => {
    if (readOnly || busyKey !== null) return;
    setBusyKey(`${verb}:${card.id}`);
    const res = await decideAccessRequest(
      card.id,
      verb,
      verb === "approve" ? { scope: card.requestedScope, duration: "ongoing" } : undefined,
    );
    setBusyKey(null);
    if (res.kind === "done") {
      onToast("ok", verb === "approve" ? t("Access granted.") : t("Request declined."));
      void homeDecisions.backgroundReload();
      await load();
    } else if (res.kind === "already") {
      onToast("warn", t("This request was already decided."));
      void homeDecisions.backgroundReload();
      await load();
    } else {
      onToast("err", res.kind === "refused" ? res.message : t("Could not update request."));
    }
  };

  return (
    <section
      className="card p-4 grid gap-3 mb-4"
      data-wiki-access-panel
      aria-label={t("Wiki path access")}
    >
      <div className="flex items-center justify-between gap-2 flex-wrap">
        <div className="flex items-center gap-2 min-w-0">
          <strong className="text-ink-100">{t("Access")}</strong>
          <span className="font-mono text-micro text-ink-400 break-all">{path}</span>
        </div>
        <Button onClick={onClose}>{t("Close")}</Button>
      </div>

      {loading && !overview ? (
        <p className="text-label text-ink-400 m-0">{t("Loading access settings…")}</p>
      ) : error ? (
        <Note warn>{error}</Note>
      ) : overview ? (
        <>
          <div className="text-label text-ink-300" data-wiki-access-inherited>
            <strong className="text-ink-200">{t("Inherited policy")}:</strong>{" "}
            {overview.inherited_policy}
          </div>

          {!overview.grantable ? (
            <div data-wiki-access-locked-reason>
              <Note warn>
                {overview.reason ?? t("Member access grants cannot be configured on this path.")}
              </Note>
            </div>
          ) : (
            <>
              {pendingCards.length > 0 && (
                <div className="grid gap-2" data-wiki-access-pending>
                  <div className="text-label font-medium text-ink-200">
                    {t("Pending requests")} ({pendingCards.length})
                  </div>
                  {pendingCards.map((card) => (
                    <div
                      key={card.id}
                      className="flex flex-wrap items-center justify-between gap-2 p-2.5 rounded border border-ink-700 bg-ink-850"
                      data-wiki-pending-id={card.id}
                    >
                      <div className="grid gap-0.5 min-w-0">
                        <span className="text-secondary text-ink-100">
                          {card.targetLabel} · {scopeShortLabel(card.requestedScope, t)}
                        </span>
                        {card.reason && (
                          <span className="text-micro text-ink-400 break-words">“{card.reason}”</span>
                        )}
                      </div>
                      <div className="flex items-center gap-2">
                        <Button
                          variant="primary"
                          disabled={readOnly || busyKey !== null}
                          onClick={() => void decideInline(card, "approve")}
                        >
                          {t("Approve")}
                        </Button>
                        <Button
                          disabled={readOnly || busyKey !== null}
                          onClick={() => void decideInline(card, "decline")}
                        >
                          {t("Decline")}
                        </Button>
                      </div>
                    </div>
                  ))}
                </div>
              )}

              <div className="grid gap-2" data-wiki-access-grants>
                <div className="text-label font-medium text-ink-200">
                  {t("Explicit member grants")}
                </div>
                {overview.grants.length === 0 ? (
                  <p className="text-label text-ink-400 m-0">
                    {t("No explicit member grants on this path.")}
                  </p>
                ) : (
                  <div className="grid gap-1.5">
                    {overview.grants.map((grant) => (
                      <div
                        key={`${grant.path}:${grant.subject_id}`}
                        className="flex flex-wrap items-center justify-between gap-2 p-2 rounded border border-ink-700 bg-ink-850"
                        data-wiki-grant-row={`${grant.path}:${grant.subject_id}`}
                      >
                        <div className="flex flex-wrap items-center gap-2 min-w-0">
                          <span className="text-secondary text-ink-100">
                            {personLabel(grant.subject_id)}
                          </span>
                          <span className="chip bg-ink-800 text-ink-300">
                            {grant.mode === "write" ? t("Read & Write") : t("Read-only")}
                          </span>
                          <span className="text-micro text-ink-400">
                            {grant.expires_at
                              ? `${t("Until")} ${new Date(grant.expires_at).toLocaleDateString()}`
                              : t("Ongoing")}
                          </span>
                          {grant.inherited && (
                            <span className="text-micro text-ink-500">
                              ({t("Inherited from")} {grant.path})
                            </span>
                          )}
                        </div>
                        {!grant.inherited && (
                          <Button
                            disabled={readOnly || busyKey !== null}
                            onClick={() => void revokeGrant(grant)}
                          >
                            {t("Revoke")}
                          </Button>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>

              <form
                className="flex flex-wrap items-end gap-2 pt-2 border-t border-ink-700"
                data-wiki-access-form
                onSubmit={(e) => void addGrant(e)}
              >
                <label className="grid gap-1 text-micro text-ink-300">
                  <span>{t("Member")}</span>
                  <select
                    className="field"
                    value={subjectId}
                    disabled={readOnly || memberOptions.length === 0}
                    onChange={(e) => setSubjectId(e.target.value)}
                    data-wiki-grant-member
                  >
                    {memberOptions.length === 0 ? (
                      <option value="">{t("No members loaded")}</option>
                    ) : (
                      memberOptions.map((m) => (
                        <option key={m.subject} value={m.subject}>
                          {m.label}
                        </option>
                      ))
                    )}
                  </select>
                </label>

                <label className="grid gap-1 text-micro text-ink-300">
                  <span>{t("Access")}</span>
                  <select
                    className="field"
                    value={mode}
                    disabled={readOnly}
                    onChange={(e) => setMode(e.target.value as "read" | "write")}
                    data-wiki-grant-mode
                  >
                    <option value="read">{t("Read-only")}</option>
                    <option value="write">{t("Read & Write")}</option>
                  </select>
                </label>

                <label className="grid gap-1 text-micro text-ink-300">
                  <span>{t("Duration")}</span>
                  <select
                    className="field"
                    value={duration}
                    disabled={readOnly}
                    onChange={(e) => setDuration(e.target.value as AccessRequestDuration)}
                    data-wiki-grant-duration
                  >
                    <option value="ongoing">{durationLabel("ongoing", t)}</option>
                    <option value="7d">{durationLabel("7d", t)}</option>
                    <option value="30d">{durationLabel("30d", t)}</option>
                  </select>
                </label>

                <button
                  type="submit"
                  className="btn btn-primary btn-sm"
                  disabled={readOnly || !subjectId || busyKey !== null}
                  data-wiki-grant-submit
                >
                  {busyKey === "add" ? t("Saving…") : t("Grant access")}
                </button>
              </form>
            </>
          )}
        </>
      ) : null}
    </section>
  );
}
