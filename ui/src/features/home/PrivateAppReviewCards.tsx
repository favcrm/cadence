import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed } from "../../lib/privateSession";
import { PrivateAppDialog } from "../workspace-apps/PrivateAppDialog";
import {
  decideHostedPrivateReview,
  grantPrivateAppUpdatePolicy,
  listHostedPrivateReviews,
  type HostedPrivateReview,
} from "../workspace-apps/privateAppUpload";
import { notifyAppsChanged } from "../workspace-apps/workspaceApps";

const THIRTY_DAYS_MS = 30 * 24 * 60 * 60 * 1000;

type CardFeedback = {
  kind: "error" | "installed" | "policy_granted";
  message: string;
  requestId?: string;
};

function accessSummary(review: HostedPrivateReview["review"], t: (english: string) => string): string {
  const conns = review.declared.connections.length;
  const caps = Object.keys(review.declared.capabilities).length;
  if (conns === 0 && caps === 0) return t("No workspace connections or extra permissions requested.");
  return t("{connections} connections · {capabilities} capabilities")
    .replace("{connections}", () => String(conns))
    .replace("{capabilities}", () => String(caps));
}

function ReviewCard({
  item,
  onOpenDialog,
  onUpdated,
  onRemoved,
}: {
  item: HostedPrivateReview;
  onOpenDialog: (id: string) => void;
  onUpdated: (next: HostedPrivateReview) => void;
  onRemoved: (id: string) => void;
}) {
  const t = useTranslate();
  const [busy, setBusy] = useState<"install" | "reject" | "policy" | null>(null);
  const [feedback, setFeedback] = useState<CardFeedback | null>(null);

  const installId = item.installedInstallId ?? item.review.target?.installId;
  const canInlineApprove = item.state === "pending" && !item.catalogStale && item.review.compatible && !item.review.warnings;
  const revokesGrants = item.willRevokeGrants ?? Boolean(item.review.target && item.review.digest !== item.review.target.digest);

  const act = async (decision: "install" | "reject") => {
    if (busy !== null || item.state !== "pending") return;
    setBusy(decision);
    setFeedback(null);
    try {
      const next = await decideHostedPrivateReview(item, decision);
      if (decision === "reject" || next.state === "rejected" || next.state === "expired") {
        onRemoved(item.id);
        return;
      }
      onUpdated(next);
      if (next.state === "installed") {
        notifyAppsChanged();
        setFeedback({ kind: "installed", message: t("Installed.") });
      }
    } catch (cause) {
      const msg = cause instanceof ApiError && cause.status === 403
        ? t("Only a current workspace owner can review this request.")
        : cause instanceof ApiError && cause.status === 409 &&
            (cause.code === "installation_stale" || cause.code === "app_exists" || cause.code === "package_changed")
          ? t("Workspace apps changed after this review was created. Ask the requester for a new review.")
          : cause instanceof ApiError && cause.message
            ? cause.message
            : t("The decision did not finish.");
      setFeedback({
        kind: "error",
        message: msg,
        requestId: cause instanceof ApiError ? cause.requestId : undefined,
      });
    } finally {
      setBusy(null);
    }
  };

  const allowRoutine = async () => {
    if (busy !== null || !installId || !item.requestingAgent?.startsWith("agents/")) return;
    setBusy("policy");
    setFeedback(null);
    try {
      await grantPrivateAppUpdatePolicy({
        installId,
        expectedDigest: item.review.digest,
        agent: item.requestingAgent,
        source: "staged_zip",
        expiresAt: Date.now() + THIRTY_DAYS_MS,
      });
      setFeedback({
        kind: "policy_granted",
        message: t("Routine patch permission saved for 30 days."),
      });
    } catch (cause) {
      setFeedback({
        kind: "error",
        message: cause instanceof ApiError && cause.message
          ? cause.message
          : t("Routine update permission could not be saved."),
        requestId: cause instanceof ApiError ? cause.requestId : undefined,
      });
    } finally {
      setBusy(null);
    }
  };

  return (
    <article className="app-assistant-card min-w-0" data-private-app-review={item.id}>
      <header className="app-assistant-card-head">
        <strong className="text-label text-ink-100 [overflow-wrap:anywhere]">
          <bdi>{item.review.title}</bdi> · <span className="num">v{item.review.version}</span>
        </strong>
        <span className="text-micro text-ink-400">
          {item.state === "installed"
            ? t("Installed")
            : item.catalogStale
              ? t("Needs new review")
              : item.authorizedBy
                ? t("Authorized for agent apply")
                : t("Private app review")}
        </span>
      </header>
      {item.requestingAgent && (
        <p className="text-micro text-ink-400">
          {t("Requested by")} <code className="text-ink-200">{item.requestingAgent}</code>
        </p>
      )}
      <p className="text-label text-ink-300">{accessSummary(item.review, t)}</p>
      {item.state === "pending" && revokesGrants && (
        <p className="text-micro text-warn">
          {typeof item.activeBindingsCount === "number" && item.activeBindingsCount > 0
            ? t("Updating this app revokes {count} connection or tool grants. Reconnect them after the update.")
                .replace("{count}", () => String(item.activeBindingsCount))
            : t("Updating this app revokes its connection and tool grants. Reconnect them after the update.")}
        </p>
      )}
      {item.state === "pending" && item.review.warnings && (
        <p className="text-micro text-fail" role="alert">
          {t("The package reported warnings. Resolve them before installing.")}
        </p>
      )}
      {item.state === "pending" && !item.review.compatible && (
        <p className="text-micro text-fail" role="alert">
          {t("This package is not compatible with this workspace.")}
        </p>
      )}
      {feedback && (
        <p className={`text-label ${feedback.kind === "error" ? "text-fail" : "text-ink-200"}`} role={feedback.kind === "error" ? "alert" : "status"}>
          {feedback.message}
          {feedback.requestId && <span className="ml-2 text-micro text-ink-400 num">Ref: {feedback.requestId}</span>}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        {canInlineApprove && (
          <button
            type="button"
            className="btn btn-sm btn-primary"
            disabled={busy !== null}
            onClick={() => void act("install")}
          >
            {busy === "install" ? t("Installing…") : t("Approve and install")}
          </button>
        )}
        {item.state === "pending" && (
          <>
            <button type="button" className="btn btn-sm" disabled={busy !== null} onClick={() => onOpenDialog(item.id)}>
              {t("Review details")}
            </button>
            <a className="btn btn-sm btn-ghost" href={`/apps?review=${encodeURIComponent(item.id)}`}>
              {t("Open in Apps")}
            </a>
            <button type="button" className="btn btn-sm" disabled={busy !== null} onClick={() => void act("reject")}>
              {busy === "reject" ? t("Rejecting…") : t("Reject")}
            </button>
          </>
        )}
        {item.state === "installed" && (
          <>
            {item.requestingAgent?.startsWith("agents/") && installId && feedback?.kind !== "policy_granted" && (
              <button type="button" className="btn btn-sm" disabled={busy !== null} onClick={() => void allowRoutine()}>
                {busy === "policy" ? t("Saving…") : t("Allow routine patch updates (30 days)")}
              </button>
            )}
            <button type="button" className="btn btn-sm btn-ghost" onClick={() => onRemoved(item.id)}>
              {t("Dismiss")}
            </button>
          </>
        )}
      </div>
    </article>
  );
}

/** Inline workspace-owner private app approval cards on Home; hides on unsupported/404 hosts. */
export default function PrivateAppReviewCards({ enabled, revision }: { enabled: boolean; revision: number }) {
  const t = useTranslate();
  const [reviews, setReviews] = useState<HostedPrivateReview[]>([]);
  const [supported, setSupported] = useState(true);
  const [dialogId, setDialogId] = useState<string | null>(null);
  const epoch = useRef(0);

  const load = useCallback((signal?: AbortSignal) => {
    if (!enabled || !privateReadsAllowed()) return;
    const current = epoch.current;
    void listHostedPrivateReviews(signal).then((result) => {
      if (signal?.aborted || epoch.current !== current || !privateReadsAllowed()) return;
      setSupported(result.supported);
      setReviews(result.reviews);
    }, () => {
      if (!signal?.aborted && epoch.current === current) setReviews([]);
    });
  }, [enabled]);

  useEffect(() => {
    if (!enabled) {
      setReviews([]);
      return;
    }
    const controller = new AbortController();
    load(controller.signal);
    const reset = () => {
      epoch.current += 1;
      controller.abort();
      setReviews([]);
      setDialogId(null);
    };
    const stopPause = onPrivateReadsPaused(reset);
    const stopReset = onPrivateSessionReset(reset);
    return () => {
      epoch.current += 1;
      controller.abort();
      stopPause();
      stopReset();
    };
  }, [enabled, revision, load]);

  if (!enabled || !supported || (reviews.length === 0 && dialogId === null)) return null;

  return (
    <>
      {reviews.length > 0 && (
        <section className="app-handoff-list" aria-label={t("Pending private app reviews")}>
          {reviews.map((item) => (
            <ReviewCard
              key={item.id}
              item={item}
              onOpenDialog={(id) => setDialogId(id)}
              onUpdated={(next) => setReviews((prev) => prev.map((row) => (row.id === next.id ? next : row)))}
              onRemoved={(id) => setReviews((prev) => prev.filter((row) => row.id !== id))}
            />
          ))}
        </section>
      )}
      {dialogId !== null && (
        <PrivateAppDialog
          reviewId={dialogId}
          onClose={() => {
            setDialogId(null);
            load();
          }}
          onInstalled={() => load()}
        />
      )}
    </>
  );
}
