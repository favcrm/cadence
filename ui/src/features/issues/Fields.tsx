import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import type { IssueCard, IssueDetail } from "../../lib/types";
import Select from "../../ui/Select";
import { noDragReason } from "../projects/Card";
import { useLocale } from "../../lib/locale";
import { type IssuePageProps as Props } from "./issuePageProps";

const STATUSES = ["backlog", "ready", "doing", "review", "done", "dropped"];
const PRIORITIES = ["P0", "P1", "P2", "P3"];

type SavingField = "status" | "priority" | "owner" | "epic" | null;

export function Fields({
  detail,
  epics,
  readOnly,
  onWrite,
  onError,
  onPatch,
}: {
  detail: IssueDetail;
  epics: IssueCard[];
  readOnly: boolean;
  onWrite: Props["onWrite"];
  onError: Props["onError"];
  onPatch: (body: Parameters<typeof api.patch>[1], verb: string) => Promise<void> | void;
}) {
  const { t } = useLocale();
  const locked = noDragReason(detail);
  const [owner, setOwner] = useState(detail.owner ?? "");
  const [savingField, setSavingField] = useState<SavingField>(null);
  const ownerSavingRef = useRef(false);
  useEffect(() => setOwner(detail.owner ?? ""), [detail.owner, detail.rev]);

  const runPatch = (field: Exclude<SavingField, null>, body: Parameters<typeof api.patch>[1], verb: string) => {
    if (field === "owner") ownerSavingRef.current = true;
    setSavingField(field);
    void Promise.resolve(onPatch(body, verb)).finally(() => {
      if (field === "owner") ownerSavingRef.current = false;
      setSavingField((current) => (current === field ? null : current));
    });
  };

  const commitOwner = () => {
    if (ownerSavingRef.current) return;
    if ((detail.owner ?? "") !== owner) {
      runPatch("owner", { owner }, `${detail.id} owner`);
    }
  };

  const setEpic = (next: string) => {
    const prev = detail.parent;
    const chain = prev
      ? api.unlink(detail.id, "parent", prev, detail.rev).then((r) => {
          if (!next) return r;
          return api.link(detail.id, "parent", next, r.issue.rev);
        })
      : next
        ? api.link(detail.id, "parent", next, detail.rev)
        : Promise.resolve(null);
    setSavingField("epic");
    void chain
      .then((r) => {
        if (r) onWrite(r, `${detail.id} epic`);
      })
      .catch((e) => onError(e, "epic"))
      .finally(() => {
        setSavingField((current) => (current === "epic" ? null : current));
      });
  };
  return (
    <section className="issue-fields grid grid-cols-[minmax(0,1fr)] gap-3" aria-label={t("Fields")}>
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-cardtitle font-semibold text-ink-100 m-0">{t("Fields")}</h2>
        {savingField && (
          <span className="text-micro text-ink-400" role="status">
            {t("Saving…")}
          </span>
        )}
      </div>
      <label className="issue-field">
        <span>{t("Status")}</span>
        <Select
          full
          value={detail.status}
          disabled={readOnly || !!locked}
          title={locked ?? undefined}
          aria-label={t("Status")}
          options={[
            ...(STATUSES.includes(detail.status)
              ? []
              : [{ value: detail.status, label: t(detail.status) } ]),
            ...STATUSES.map((s) => ({ value: s, label: t(s) })),
          ]}
          onChange={(status) => runPatch("status", { status }, `${detail.id} status`)}
        />
      </label>
      <label className="issue-field">
        <span>{t("Priority")}</span>
        <Select
          full
          value={detail.priority}
          disabled={readOnly}
          aria-label={t("Priority")}
          options={[
            ...(PRIORITIES.includes(detail.priority)
              ? []
              : [{ value: detail.priority, label: detail.priority }]),
            ...PRIORITIES.map((p) => ({ value: p, label: p })),
          ]}
          onChange={(priority) =>
            runPatch("priority", { priority }, `${detail.id} priority`)
          }
        />
      </label>
      <label className="issue-field">
        <span>{t("Owner")}</span>
        <input
          className="field w-full"
          value={owner}
          disabled={readOnly}
          aria-label={t("Owner")}
          onChange={(e) => setOwner(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commitOwner();
              e.currentTarget.blur();
            }
          }}
          onBlur={commitOwner}
        />
      </label>
      <label className="issue-field">
        <span>{t("Epic")}</span>
        <Select
          full
          value={detail.parent ?? ""}
          disabled={readOnly}
          aria-label={t("Epic")}
          options={[
            { value: "", label: t("None") },
            ...(detail.parent && !epics.some((e) => e.id === detail.parent)
              ? [{ value: detail.parent, label: detail.parent }]
              : []),
            ...epics.map((e) => ({
              value: e.id,
              label: `${e.id} · ${e.title}`,
            })),
          ]}
          onChange={setEpic}
        />
      </label>
    </section>
  );
}
