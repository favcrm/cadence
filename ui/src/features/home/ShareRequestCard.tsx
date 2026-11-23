import { useContext, useEffect, useState } from "react";
import { useLocaleTag, useTranslate } from "../../lib/locale";
import { requestThreadReload } from "../../lib/masterChanged";
import { TurnCard, type CardDensity, type CardStatus } from "../../ui/TurnCard";
import { WriteGate } from "../auth/WriteGate";
import { optionsLine } from "../canvas/viewParams";
import { plainText } from "./chatCards";
import { alreadyText, declinedLine, decideShareRequest, parseShareRequestEntry, readShareRequest, type ShareCardModel } from "./shareRequest";
import type { ThreadEntry } from "./thread";

/**
 * AOS-711 (CAD-2705): the host's `share_request` card in the member's thread.
 * The request is the app, the view and its params, and the member's own
 * reason; nothing here grants access. Pending shows Cancel request. When an
 * owner shares it, the file is the `artifact` card that follows this one, and
 * the existing viewer and download open it. Every value is plain text.
 *
 * The thread is rewritten in place when a request is decided, and the member's
 * forward poll does not re-read a rewritten row, so a pending card reads
 * `GET /api/share-requests/:id` itself and asks the chat to reload when the
 * answer moved.
 */
const POLL_MS = 15_000;

const STATUS: Record<ShareCardModel["status"], { text: string; tone: CardStatus["tone"] }> = {
  pending: { text: "Pending", tone: "warn" },
  shared: { text: "Shared", tone: "ok" },
  declined: { text: "Declined", tone: "muted" },
  cancelled: { text: "Cancelled", tone: "muted" },
};

/** The card's current truth: what the thread says, until a fresher read of the same request says otherwise. */
function useLiveRequest(model: ShareCardModel | null): [ShareCardModel | null, (next: ShareCardModel) => void] {
  const [live, setLive] = useState<ShareCardModel | null>(null);
  const id = model?.id ?? null;
  const pending = (live ?? model)?.status === "pending";
  useEffect(() => {
    if (id === null || !pending) return;
    let stopped = false;
    const read = () => {
      if (document.visibilityState === "hidden") return;
      void readShareRequest(id).then((next) => {
        if (stopped || next === null || next.status === "pending") return;
        setLive(next);
        requestThreadReload();
      });
    };
    read();
    const timer = setInterval(read, POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [id, pending]);
  // The thread caught up: its row is at least as new as ours.
  const current = live !== null && model !== null && live.id === model.id && model.status === "pending" ? live : model;
  return [current, setLive];
}

export function ShareRequestCard({ entry, density = "full" }: { entry: ThreadEntry; density?: CardDensity }) {
  const t = useTranslate();
  const locale = useLocaleTag();
  const writeBlock = useContext(WriteGate);
  const model = parseShareRequestEntry(entry);
  const [request, setRequest] = useLiveRequest(model);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  if (!model || !request) return <p className="text-micro text-ink-500 break-words chatcard-line" data-kind="share_request">· {plainText(entry.text, 500)}</p>;

  const cancel = async () => {
    if (busy || request.status !== "pending") return;
    setBusy(true);
    setNote(null);
    const outcome = await decideShareRequest(request.id, "cancel");
    if (outcome.kind === "done") {
      setRequest({ ...request, status: "cancelled", decidedAt: new Date().toISOString() });
      requestThreadReload();
    } else if (outcome.kind === "already") {
      setNote(alreadyText(outcome, t));
      const fresh = await readShareRequest(request.id);
      if (fresh !== null) setRequest(fresh);
      requestThreadReload();
    } else {
      setNote(outcome.kind === "missing" ? t("This request is no longer available.")
        : outcome.kind === "access" ? t("You can't cancel this request.")
        : t("Couldn't cancel this request. Try again."));
    }
    setBusy(false);
  };

  const options = optionsLine(request.params, locale, t);
  const status = STATUS[request.status];
  const owner = t("An owner");
  const outcome =
    request.status === "pending" ? t("Waiting for an owner. You'll hear back here.")
    : request.status === "shared" ? t("Shared by {name}").replace("{name}", () => request.decidedBy ?? owner)
    : request.status === "declined" ? declinedLine(request, t, owner)
    : t("You cancelled this request.");
  return (
    <TurnCard
      kind="share"
      className="card chatcard sharecard"
      bodyClassName="chatcard-body"
      density={density}
      data-kind="share_request"
      data-share-state={request.status}
      label={t("Share request card")}
      kindLabel={t("Share request")}
      subject={<span className="viewcard-title" title={request.title}>{request.title}</span>}
      status={{ text: t(status.text), tone: status.tone }}
      footer={
        request.status === "pending" && writeBlock === null ? (
          <button type="button" className="btn btn-ghost" data-share-cancel aria-busy={busy || undefined} disabled={busy} onClick={() => void cancel()}>
            {busy ? t("Cancelling…") : t("Cancel request")}
          </button>
        ) : null
      }
    >
      <p className="text-label text-ink-300 viewcard-place" title={`${request.appLabel} · ${request.viewLabel}`}>{request.appLabel} · {request.viewLabel}</p>
      {options === "" ? null : <p className="text-label text-ink-300 viewcard-params" data-share-options>{options}</p>}
      {request.reason === undefined ? null : <p className="text-label text-ink-300 break-words" data-share-reason>“{request.reason}”</p>}
      <p className="text-label text-ink-200 break-words" data-share-outcome role="status">{outcome}</p>
      {note === null ? null : <p className="text-label text-warn break-words" role="alert">{note}</p>}
    </TurnCard>
  );
}
