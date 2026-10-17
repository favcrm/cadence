import { useCallback, useEffect, useState } from "react";
import { api, ApiError, type MyWorkItem, type MyWorkPayload } from "../../lib/api";
import { issuePath } from "../issues/model";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import PageState from "../../ui/PageState";

const STATE_LABEL: Record<MyWorkItem["state"], string> = {
  queued: "Queued",
  running: "Running",
  blocked: "Blocked",
  result: "Result",
  failed: "Failed",
  cancelled: "Cancelled",
};

function messageOf(error: unknown): string {
  if (error instanceof ApiError) return error.message;
  if (error instanceof Error && error.message) return error.message;
  return "Hosted work could not be confirmed.";
}

function Row({ item }: { item: MyWorkItem }) {
  const t = useTranslate();
  const label = STATE_LABEL[item.state] ?? item.state;
  const attribution = item.assigned_to_you && item.created_by_you
    ? "Assigned to you · created by you"
    : item.assigned_to_you
      ? "Assigned to you"
      : "Created by you";
  return (
    <li className="flex flex-col gap-1 border-b border-line py-3 sm:flex-row sm:items-baseline sm:justify-between">
      <div className="min-w-0">
        <Link href={issuePath(item.project, item.issue_id)}>{item.issue_id}</Link>
        <p className="text-sm text-ink-300">{t(attribution)} · {item.action}</p>
      </div>
      <p className="text-sm text-ink-200">{t(label)}</p>
    </li>
  );
}

function Section({ title, items }: { title: string; items: MyWorkItem[] }) {
  const t = useTranslate();
  if (items.length === 0) return null;
  return (
    <section className="mt-6">
      <h2 className="text-section font-semibold text-ink-100">{t(title)}</h2>
      <ul>
        {items.map((item) => (
          <Row key={`${title}-${item.id}`} item={item} />
        ))}
      </ul>
    </section>
  );
}

/** The signed-in member's hosted assignments. Claim is not shown as execution. */
export default function MyWork() {
  const t = useTranslate();
  const [payload, setPayload] = useState<MyWorkPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    api.myWork()
      .then((body) => {
        setPayload(body);
        setLoading(false);
      })
      .catch((err: unknown) => {
        setPayload(null);
        setError(messageOf(err));
        setLoading(false);
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const refresh = (
    <Button type="button" onClick={load} loading={loading}>
      {t("Refresh")}
    </Button>
  );

  if (loading && !payload && !error) {
    return (
      <div className="px-4 py-6 lg:px-8">
        <PageState title={t("My work")}>{t("Loading your assignments.")}</PageState>
      </div>
    );
  }
  if (error) {
    return (
      <div className="px-4 py-6 lg:px-8">
        <PageState title={t("My work unavailable")} actions={refresh}>{error}</PageState>
      </div>
    );
  }
  const assigned = payload?.assigned ?? [];
  const created = payload?.created ?? [];
  if (assigned.length === 0 && created.length === 0) {
    return (
      <div className="px-4 py-6 lg:px-8">
        <PageState title={t("My work")} actions={refresh}>
          {t("Nothing is assigned to you or created by you.")}
        </PageState>
      </div>
    );
  }
  return (
    <div className="px-4 py-6 lg:px-8">
      <div className="flex items-center justify-between gap-3">
        <h1 className="text-section font-semibold text-ink-100">{t("My work")}</h1>
        {refresh}
      </div>
      <p className="mt-2 text-sm text-ink-300">
        {t("Queued, blocked, result, failed and cancelled describe stored custody. A claim is not running work.")}
      </p>
      <Section title="Assigned to me" items={assigned} />
      <Section title="Created by me" items={created} />
    </div>
  );
}
