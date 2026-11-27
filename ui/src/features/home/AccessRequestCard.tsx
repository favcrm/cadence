import { useContext, useEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { requestThreadReload } from "../../lib/masterChanged";
import { useVisiblePoll } from "../../lib/useVisiblePoll";
import Link from "../../ui/Link";
import { TurnCard, type CardDensity, type CardStatus } from "../../ui/TurnCard";
import { WriteGate } from "../auth/WriteGate";
import {
  accessTargetHref,
  alreadyAccessText,
  confirmAccessOffer,
  declinedAccessLine,
  decideAccessRequest,
  durationLabel,
  parseAccessOfferEntry,
  parseAccessRequestEntry,
  readAccessRequest,
  scopeShortLabel,
  type AccessCardModel,
  type AccessOffer,
  type ConfirmAccessOfferOutcome,
} from "./accessRequest";
import { plainText } from "./chatCards";
import type { ThreadEntry } from "./thread";

/**
 * AOS-816 (CAD-2885): member's `access_request` card in Home chat.
 * Mirrors `ShareRequestCard`:
 *  - Host-authored offers require an explicit member click (`Send request`) to reach owners.
 *  - Pending requests poll `GET /api/access-requests/:id` while visible and offer `Cancel request`.
 *  - Approved requests show granted scope + duration and navigate directly to the unlocked project or wiki path.
 */
const POLL_MS = 15_000;

const STATUS: Record<AccessCardModel["status"], { text: string; tone: CardStatus["tone"] }> = {
  pending: { text: "Pending", tone: "warn" },
  approved: { text: "Approved", tone: "ok" },
  declined: { text: "Declined", tone: "muted" },
  cancelled: { text: "Cancelled", tone: "muted" },
};

function useLiveAccessRequest(
  model: AccessCardModel | null,
): [AccessCardModel | null, (next: AccessCardModel) => void] {
  const [live, setLive] = useState<AccessCardModel | null>(null);
  const id = model?.id ?? null;
  const pending = (live ?? model)?.status === "pending";
  const activeIdRef = useRef<string | null>(null);

  useEffect(() => {
    activeIdRef.current = id !== null && pending ? id : null;
    return () => {
      activeIdRef.current = null;
    };
  }, [id, pending]);

  useVisiblePoll(
    id !== null && pending,
    POLL_MS,
    () => {
      if (id === null) return;
      void readAccessRequest(id).then((next) => {
        if (activeIdRef.current !== id || next === null || next.status === "pending") return;
        setLive(next);
        requestThreadReload();
      });
    },
    { immediate: true, onFocus: true },
  );

  const current =
    live !== null && model !== null && live.id === model.id && model.status === "pending" ? live : model;
  return [current, setLive];
}

function AccessOfferCard({ offer, density }: { offer: AccessOffer; density: CardDensity }) {
  const t = useTranslate();
  const writeBlock = useContext(WriteGate);
  const [result, setResult] = useState<ConfirmAccessOfferOutcome | null>(null);
  const [busy, setBusy] = useState(false);
  const [expired, setExpired] = useState(() => Date.parse(offer.expiresAt) <= Date.now());
  const inFlight = useRef(false);

  useEffect(() => {
    let timer = 0;
    const schedule = () => {
      const delay = Date.parse(offer.expiresAt) - Date.now();
      if (delay <= 0) {
        setExpired(true);
        return;
      }
      timer = window.setTimeout(schedule, Math.min(delay, 2_147_483_647));
    };
    schedule();
    return () => window.clearTimeout(timer);
  }, [offer.expiresAt]);

  const state =
    offer.status === "offered"
      ? result ?? (expired ? "expired" : "offered")
      : offer.status === "submitting"
        ? result ?? "submitting"
        : offer.status;
  const check = state === "submitting" || state === "unknown";
  const actionable = state === "offered" || check;

  const confirm = async () => {
    if (
      inFlight.current ||
      busy ||
      !actionable ||
      writeBlock !== null ||
      (state === "offered" && Date.parse(offer.expiresAt) <= Date.now())
    ) {
      return;
    }
    inFlight.current = true;
    setBusy(true);
    const next = await confirmAccessOffer(offer);
    setResult(check && next === "unavailable" ? "unknown" : next);
    inFlight.current = false;
    setBusy(false);
    requestThreadReload();
  };

  const messages: Record<AccessOffer["status"] | ConfirmAccessOfferOutcome, string> = {
    offered: "Access request not sent yet. Send it to ask an owner to review.",
    submitting: "The result is unknown. Check this same request without sending a duplicate.",
    sent: "Waiting for an owner. You'll hear back here.",
    already_requested: "An access request for this target is already pending.",
    expired: "This offer has expired. Ask again if you still need access.",
    failed: "This request could not be sent. Ask again.",
    missing: "This offer is no longer available.",
    inactive: "This offer is no longer active.",
    unavailable: "The request could not be confirmed or owners are unavailable.",
    access: "You can't send this request.",
    unknown: "The result is unknown. Check this same request without sending a duplicate.",
  };

  const statusText = busy
    ? "Sending"
    : check
      ? "Delivery unconfirmed"
      : state === "sent"
        ? "Pending"
        : state === "already_requested"
          ? "Already pending"
          : state === "offered"
            ? "Not sent"
            : state === "expired"
              ? "Expired"
              : "Unavailable";

  return (
    <TurnCard
      kind="share"
      className="card chatcard sharecard"
      bodyClassName="chatcard-body"
      density={density}
      data-kind="access_request"
      data-access-state={busy ? "submitting" : state}
      label={t("Access request card")}
      kindLabel={t("Access request")}
      subject={
        <span className="viewcard-title" title={offer.targetLabel}>
          {offer.targetLabel}
        </span>
      }
      status={{
        text: t(statusText),
        tone: state === "sent" || state === "already_requested" ? "ok" : "warn",
      }}
      footer={
        writeBlock === null ? (
          <button
            type="button"
            className="btn btn-primary"
            data-access-confirm
            aria-busy={busy || undefined}
            disabled={busy || !actionable}
            onClick={() => void confirm()}
          >
            {t(busy ? "Sending request…" : check ? "Check request" : "Send request")}
          </button>
        ) : null
      }
    >
      <p className="text-label text-ink-300 viewcard-place" data-access-scope>
        {scopeShortLabel(offer.requestedScope, t)}
      </p>
      {offer.reason === undefined ? null : (
        <p className="text-label text-ink-300 break-words" data-access-reason>
          “{offer.reason}”
        </p>
      )}
      <p className="text-label text-ink-200 break-words" data-access-outcome role="status">
        {t(busy ? "Sending request…" : messages[state])}
      </p>
      {writeBlock === null ? null : <p className="text-label text-warn break-words">{writeBlock}</p>}
    </TurnCard>
  );
}

function RegularAccessRequestCard({ entry, density = "full" }: { entry: ThreadEntry; density?: CardDensity }) {
  const t = useTranslate();
  const writeBlock = useContext(WriteGate);
  const model = parseAccessRequestEntry(entry);
  const [request, setRequest] = useLiveAccessRequest(model);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  if (!model || !request) {
    return (
      <p className="text-micro text-ink-500 break-words chatcard-line" data-kind="access_request">
        · {plainText(entry.text, 500)}
      </p>
    );
  }

  const cancel = async () => {
    if (busy || request.status !== "pending") return;
    setBusy(true);
    setNote(null);
    const outcome = await decideAccessRequest(request.id, "cancel");
    if (outcome.kind === "done") {
      setRequest({ ...request, status: "cancelled", decidedAt: new Date().toISOString() });
      requestThreadReload();
    } else if (outcome.kind === "already") {
      setNote(alreadyAccessText(outcome, t));
      const fresh = await readAccessRequest(request.id);
      if (fresh !== null) setRequest(fresh);
      requestThreadReload();
    } else {
      setNote(
        outcome.kind === "missing"
          ? t("This request is no longer available.")
          : outcome.kind === "access"
            ? t("You can't cancel this request.")
            : t("Couldn't cancel this request. Try again."),
      );
    }
    setBusy(false);
  };

  const status = STATUS[request.status];
  const owner = t("An owner");
  const effectiveScope = request.grantedScope ?? request.requestedScope;
  const scopeText = scopeShortLabel(effectiveScope, t);
  const durationText = request.duration ? durationLabel(request.duration, t) : null;

  const outcome =
    request.status === "pending"
      ? t("Waiting for an owner. You'll hear back here.")
      : request.status === "approved"
        ? t("Granted by {name} · {scope}")
            .replace("{name}", () => request.decidedBy ?? owner)
            .replace("{scope}", () => (durationText ? `${scopeText} (${durationText})` : scopeText))
        : request.status === "declined"
          ? declinedAccessLine(request, t, owner)
          : t("You cancelled this request.");

  const targetHref =
    request.status === "approved"
      ? accessTargetHref(request.kind, request.target, effectiveScope)
      : null;

  return (
    <TurnCard
      kind="share"
      className="card chatcard sharecard"
      bodyClassName="chatcard-body"
      density={density}
      data-kind="access_request"
      data-access-state={request.status}
      label={t("Access request card")}
      kindLabel={t("Access request")}
      subject={
        <span className="viewcard-title" title={request.targetLabel}>
          {request.targetLabel}
        </span>
      }
      status={{ text: t(status.text), tone: status.tone }}
      footer={
        request.status === "pending" && writeBlock === null ? (
          <button
            type="button"
            className="btn btn-ghost"
            data-access-cancel
            aria-busy={busy || undefined}
            disabled={busy}
            onClick={() => void cancel()}
          >
            {busy ? t("Cancelling…") : t("Cancel request")}
          </button>
        ) : targetHref ? (
          <Link className="btn btn-primary" href={targetHref} data-access-open>
            {request.kind === "project" && effectiveScope === "project_member"
              ? t("Open project")
              : t("Open in Wiki")}
          </Link>
        ) : null
      }
    >
      <p className="text-label text-ink-300 viewcard-place" data-access-scope>
        {scopeShortLabel(request.requestedScope, t)}
      </p>
      {request.reason === undefined ? null : (
        <p className="text-label text-ink-300 break-words" data-access-reason>
          “{request.reason}”
        </p>
      )}
      <p className="text-label text-ink-200 break-words" data-access-outcome role="status">
        {outcome}
      </p>
      {note === null ? null : (
        <p className="text-label text-warn break-words" role="alert">
          {note}
        </p>
      )}
    </TurnCard>
  );
}

export function AccessRequestCard({
  entry,
  density = "full",
}: {
  entry: ThreadEntry;
  density?: CardDensity;
}) {
  const offer = parseAccessOfferEntry(entry);
  return offer ? (
    <AccessOfferCard key={offer.id} offer={offer} density={density} />
  ) : (
    <RegularAccessRequestCard entry={entry} density={density} />
  );
}
