import { useLocale } from "../../lib/locale";
import type { IssueCard } from "../../lib/types";
import { PRIORITY_CHIP, STATUS_CHIP } from "./chips";
import type { IssueTab } from "./model";

export function IssuePageSkeleton({
  id,
  project,
  card,
  hosted,
  tab,
}: {
  id: string;
  project: string;
  card: IssueCard | null;
  hosted: boolean;
  tab: IssueTab;
}) {
  const { t } = useLocale();
  return (
    <div className="issue-workspace flex flex-col min-w-0" data-issue-page={id}>
      <div role="status" data-read-state="issue" className="sr-only">
        {t("Loading issue…")}
      </div>

      <header className="flex flex-wrap items-end justify-between gap-4 px-4 lg:px-8 pt-5 pb-3">
        <div className="min-w-0">
          <div className="kicker">
            <span className="num normal-case">{id}</span>
            {" · "}
            {project}
          </div>
          {card ? (
            <>
              <h1 className="issue-title font-semibold text-ink-100 tracking-tight mt-0.5">
                {card.title}
              </h1>
              <div className="flex flex-wrap items-center gap-2 mt-2">
                <span
                  className={`chip ${STATUS_CHIP[card.status] ?? "bg-ink-800 text-ink-300"}`}
                >
                  {t(card.status)}
                </span>
                <span
                  className={`chip ${PRIORITY_CHIP[card.priority] ?? "bg-ink-800 text-ink-400"}`}
                >
                  {t(card.priority)}
                </span>
                {(card.parent || card.component) && (
                  <span className="chip bg-ink-800 text-ink-400">
                    {card.parent ?? card.component}
                  </span>
                )}
              </div>
            </>
          ) : (
            <div className="issue-skeleton-surface mt-1.5 grid gap-2" aria-hidden="true">
              <span
                className="issue-skel-bar issue-skel-pulse"
                style={{ width: "min(28rem, 72vw)", height: "1.625rem" }}
              />
              <div className="flex flex-wrap items-center gap-2">
                <span className="issue-skel-chip issue-skel-pulse" style={{ width: "4.5rem" }} />
                <span className="issue-skel-chip issue-skel-pulse" style={{ width: "2.75rem" }} />
                <span className="issue-skel-chip issue-skel-pulse" style={{ width: "5.5rem" }} />
              </div>
            </div>
          )}
        </div>
        <div className="issue-skeleton-surface flex items-center gap-2 sm:ml-auto" aria-hidden="true">
          <span className="issue-skel-bar issue-skel-pulse" style={{ width: "5.5rem", height: "2rem" }} />
        </div>
      </header>

      <div
        className="issue-skeleton-surface px-4 lg:px-8 mt-3 border-b border-ink-700 flex items-center gap-5 pb-2.5"
        aria-hidden="true"
      >
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "4.5rem", height: "0.9rem" }} />
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "4rem", height: "0.9rem" }} />
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "5.5rem", height: "0.9rem" }} />
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "4.5rem", height: "0.9rem" }} />
      </div>

      <div
        className="issue-skeleton-surface grid grid-cols-[minmax(0,1fr)] xl:grid-cols-[minmax(0,1fr)_340px] items-start"
        aria-hidden="true"
      >
        <main className="min-w-0 px-4 lg:px-8 py-5 grid grid-cols-[minmax(0,1fr)] gap-4">
          {tab === "activity" ? (
            <ActivitySkeletonList />
          ) : tab === "overview" ? (
            <>
              <div className="card p-4 grid gap-2.5">
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "38%", height: "0.9rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "52%", height: "0.9rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "74%", height: "0.9rem" }} />
              </div>
              <div className="card p-4 grid gap-3">
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "6rem", height: "1.1rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "86%", height: "0.9rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "68%", height: "0.9rem" }} />
                <div className="flex gap-2 pt-1">
                  <span className="issue-skel-bar issue-skel-pulse" style={{ width: "6.5rem", height: "2rem" }} />
                  <span className="issue-skel-bar issue-skel-pulse" style={{ width: "7rem", height: "2rem" }} />
                </div>
              </div>
              <div className="grid gap-2.5 pt-1">
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "92%", height: "0.9rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "84%", height: "0.9rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "62%", height: "0.9rem" }} />
              </div>
            </>
          ) : (
            <>
              <div className="card p-4 grid gap-3">
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "9rem", height: "1.1rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "78%", height: "0.9rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "54%", height: "0.9rem" }} />
              </div>
              <div className="card p-4 grid gap-3">
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "7rem", height: "1rem" }} />
                <span className="issue-skel-bar issue-skel-pulse" style={{ width: "90%", height: "3.5rem" }} />
              </div>
            </>
          )}
        </main>

        <aside className="issue-context grid grid-cols-[minmax(0,1fr)] gap-5 px-4 lg:px-8 xl:px-6 xl:pr-8 py-5 xl:border-l xl:border-ink-700 min-w-0">
          {!hosted && <LaneCardSkeletonBody />}
          <section className="issue-fields grid grid-cols-[minmax(0,1fr)] gap-3">
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "3.5rem", height: "0.75rem" }} />
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "100%", height: "2rem" }} />
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "100%", height: "2rem" }} />
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "100%", height: "2rem" }} />
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "100%", height: "2rem" }} />
          </section>
          <section className="grid gap-2.5">
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "3rem", height: "0.75rem" }} />
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "76%", height: "0.9rem" }} />
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "62%", height: "0.9rem" }} />
          </section>
        </aside>
      </div>
    </div>
  );
}

function LaneCardSkeletonBody() {
  return (
    <section className="card p-4 grid gap-3">
      <div className="flex items-center gap-2">
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "3.5rem", height: "1rem" }} />
        <span className="issue-skel-chip issue-skel-pulse" style={{ width: "3.5rem" }} />
      </div>
      <div className="grid gap-2">
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "70%", height: "0.85rem" }} />
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "55%", height: "0.85rem" }} />
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "82%", height: "0.85rem" }} />
        <span className="issue-skel-bar issue-skel-pulse" style={{ width: "60%", height: "0.85rem" }} />
      </div>
    </section>
  );
}

export function LaneCardSkeleton() {
  const { t } = useLocale();
  return (
    <div className="issue-skeleton-surface" role="status" data-read-state="lane">
      <span className="sr-only">{t("Loading lane…")}</span>
      <div aria-hidden="true">
        <LaneCardSkeletonBody />
      </div>
    </div>
  );
}

function ActivitySkeletonList() {
  return (
    <div className="grid gap-4">
      {[0, 1, 2].map((index) => (
        <div key={index} className="relative pl-5 pb-3 grid gap-1.5">
          <span
            className="issue-skel-bar issue-skel-pulse absolute left-0 top-1.5"
            style={{ width: "0.5rem", height: "0.5rem", borderRadius: "999px" }}
          />
          <div className="flex items-center justify-between gap-3">
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "7rem", height: "0.9rem" }} />
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "5.5rem", height: "0.75rem" }} />
          </div>
          <span
            className="issue-skel-bar issue-skel-pulse"
            style={{ width: index === 1 ? "64%" : "82%", height: "0.85rem" }}
          />
        </div>
      ))}
    </div>
  );
}

export function ActivitySkeleton() {
  const { t } = useLocale();
  return (
    <div className="issue-skeleton-surface" role="status" data-read-state="history">
      <span className="sr-only">{t("Loading history…")}</span>
      <div aria-hidden="true">
        <ActivitySkeletonList />
      </div>
    </div>
  );
}

export function ConversationSkeleton() {
  const { t } = useLocale();
  return (
    <div className="issue-skeleton-surface grid gap-3" role="status" data-read-state="conversation">
      <span className="sr-only">{t("Loading conversation…")}</span>
      <div className="grid gap-3" aria-hidden="true">
        {[0, 1, 2].map((index) => (
          <div key={index} className="grid gap-1.5">
            <span className="issue-skel-chip issue-skel-pulse" style={{ width: "4rem" }} />
            <span
              className="issue-skel-bar issue-skel-pulse"
              style={{ width: index === 1 ? "72%" : "88%", height: "0.9rem" }}
            />
            <span className="issue-skel-bar issue-skel-pulse" style={{ width: "52%", height: "0.9rem" }} />
          </div>
        ))}
      </div>
    </div>
  );
}
