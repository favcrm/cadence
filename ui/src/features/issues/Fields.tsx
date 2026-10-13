import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import type { IssueCard, IssueDetail } from "../../lib/types";
import Select from "../../ui/Select";
import { noDragReason } from "../projects/Card";
import { useLocale } from "../../lib/locale";
import { type IssuePageProps as Props } from "./issuePageProps";

const STATUSES = ["backlog", "ready", "doing", "review", "done", "dropped"];
const PRIORITIES = ["P0", "P1", "P2", "P3"];

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
  onPatch: (body: Parameters<typeof api.patch>[1], verb: string) => void;
}) {
  const { t } = useLocale();
  const locked = noDragReason(detail);
  const [owner, setOwner] = useState(detail.owner ?? "");
  useEffect(() => setOwner(detail.owner ?? ""), [detail.owner, detail.rev]);
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
    chain
      .then((r) => {
        if (r) onWrite(r, `${detail.id} epic`);
      })
      .catch((e) => onError(e, "epic"));
  };
  return (
    <section className="card p-3.5 grid gap-2.5" aria-label={t("Fields")}>
      <div className="slabel">{t("Fields")}</div>
      <label className="grid gap-1.5">
        <span className="slabel">{t("Status")}</span>
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
          onChange={(status) => onPatch({ status }, `${detail.id} status`)}
        />
      </label>
      <label className="grid gap-1.5">
        <span className="slabel">{t("Priority")}</span>
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
            onPatch({ priority }, `${detail.id} priority`)
          }
        />
      </label>
      <label className="grid gap-1.5">
        <span className="slabel">{t("Owner")}</span>
        <input
          className="field w-full"
          value={owner}
          disabled={readOnly}
          aria-label={t("Owner")}
          onChange={(e) => setOwner(e.target.value)}
          onBlur={() => {
            if ((detail.owner ?? "") !== owner)
              onPatch({ owner }, `${detail.id} owner`);
          }}
        />
      </label>
      <label className="grid gap-1.5">
        <span className="slabel">{t("Epic")}</span>
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
