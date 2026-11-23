import { useCallback, useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import Button from "../../ui/Button";
import Link from "../../ui/Link";
import {
  IconCheck,
  IconConnection,
  IconInfo,
  IconLock,
  IconRefresh,
  IconThreads,
  IconWarning,
} from "../../ui/icons";
import { ApiError } from "../../lib/apiError";
import { useTranslate } from "../../lib/locale";
import { encodeQrMatrix, qrSvgPath } from "../settings/qrCode";
import {
  WhatsAppActionUncertainError,
  isWhatsAppSettingsUnsupported,
  markWhatsAppSettingsUnsupported,
  readWhatsAppSettings,
  runLinkedWhatsAppAction,
  runOfficialWhatsAppAction,
  saveWhatsAppProvider,
  shouldShowLinkedIdleNudge,
  type LinkedWhatsAppSnapshot,
  type OfficialWhatsAppSnapshot,
  type WhatsAppProvider,
  type WhatsAppSettingsSnapshot,
} from "./whatsappSettings";
import "./whatsappSettings.css";

type LinkedConfirmKind = "pause" | "disconnect" | "relink" | null;
type BadgeTone = "ok" | "warn" | "bad" | "accent" | "muted";

function chooseInitialMode(
  snapshot: WhatsAppSettingsSnapshot,
  initialMode: WhatsAppProvider | null | undefined,
): WhatsAppProvider {
  if (initialMode === "linked" || initialMode === "official") return initialMode;
  if (snapshot.active_provider === "linked" || snapshot.active_provider === "official") {
    return snapshot.active_provider;
  }
  if (snapshot.official.available && snapshot.official.connected && (!snapshot.linked.available || !snapshot.linked.connected)) {
    return "official";
  }
  return "linked";
}

function linkedBadgeMeta(
  linked: LinkedWhatsAppSnapshot,
  uncertain: boolean,
): { dot: "ok" | "warn" | "bad" | "muted"; tone: BadgeTone; tileLabel: string; cardLabel: string } {
  if (uncertain) {
    return { dot: "warn", tone: "warn", tileLabel: "Unconfirmed", cardLabel: "Unconfirmed" };
  }
  if (!linked.available) {
    return { dot: "warn", tone: "warn", tileLabel: "Unavailable", cardLabel: "Unavailable" };
  }
  switch (linked.state) {
    case "connected":
      return { dot: "ok", tone: "ok", tileLabel: "Connected", cardLabel: "Connected" };
    case "connecting":
      return { dot: "warn", tone: "warn", tileLabel: "Starting…", cardLabel: "Starting…" };
    case "pairing":
      return { dot: "warn", tone: "accent", tileLabel: "Pairing…", cardLabel: "Waiting to scan" };
    case "stopped":
      return { dot: "warn", tone: "warn", tileLabel: "Paused", cardLabel: "Paused ($0/mo)" };
    case "disconnected":
      return { dot: "warn", tone: "warn", tileLabel: "Reconnecting…", cardLabel: "Reconnecting…" };
    case "reauthorization_required":
      return { dot: "bad", tone: "bad", tileLabel: "Expired", cardLabel: "Session expired" };
    case "not_granted":
      return { dot: "muted", tone: "muted", tileLabel: "Disabled", cardLabel: "Disabled" };
    default:
      return { dot: "muted", tone: "muted", tileLabel: "Not set up", cardLabel: "Not connected" };
  }
}

function officialBadgeMeta(official: OfficialWhatsAppSnapshot): {
  dot: "ok" | "warn" | "bad" | "muted";
  tone: BadgeTone;
  tileLabel: string;
  cardLabel: string;
} {
  if (!official.available) {
    return { dot: "warn", tone: "warn", tileLabel: "Unavailable", cardLabel: "Unavailable" };
  }
  if (official.paused) {
    return { dot: "warn", tone: "warn", tileLabel: "Paused", cardLabel: "Paused" };
  }
  if (official.connected) {
    return { dot: "ok", tone: "ok", tileLabel: "Connected", cardLabel: "Connected" };
  }
  if (official.binding_status === "active") {
    return { dot: "warn", tone: "warn", tileLabel: "Pending setup", cardLabel: "Pending setup" };
  }
  return { dot: "muted", tone: "muted", tileLabel: "Not set up", cardLabel: "Not connected" };
}

function formatError(err: unknown, t: (english: string) => string, fallback: string): string {
  if (err instanceof ApiError) {
    switch (err.code) {
      case "owner_required":
        return t("Only a workspace owner may change WhatsApp settings.");
      case "revision_conflict":
        return t("Settings changed elsewhere. Reloaded the latest status — please try again.");
      case "not_granted":
        return t("Linked device pairing is not enabled for this workspace.");
      case "door_disabled":
        return t("Official WhatsApp is currently disabled for this workspace.");
      case "favcrm_not_bound":
        return t("Connect your FavCRM workspace first before linking a Business number.");
      case "grant_revoked":
        return t("The FavCRM authorization was revoked. Reconnect via FavCRM.");
      case "module_disabled":
        return t("WhatsApp is not enabled on this FavCRM company.");
      default:
        return err.message || t(fallback);
    }
  }
  if (err instanceof Error && err.message) return err.message;
  return t(fallback);
}

/**
 * Host-owned WhatsApp Business connection settings (`CAD-2718`).
 * Renders outside the sandboxed WABA iframe so pairing QR payloads and
 * privileged lifecycle controls stay strictly in Cadence's trusted host origin.
 */
export default function WhatsAppSettingsPanel({
  installId,
  initialMode = null,
  inboxHref = null,
  onUnsupported,
  onSettingsChanged,
}: {
  installId: string;
  initialMode?: WhatsAppProvider | null;
  inboxHref?: string | null;
  onUnsupported: () => void;
  onSettingsChanged?: () => void;
}) {
  const t = useTranslate();
  const titleId = useId();
  const cardHeadingId = useId();

  const [snapshot, setSnapshot] = useState<WhatsAppSettingsSnapshot | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [focusedMode, setFocusedMode] = useState<WhatsAppProvider>(initialMode ?? "linked");
  const modeInitializedRef = useRef(false);

  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [statusNotice, setStatusNotice] = useState<string | null>(null);
  const [linkedUncertain, setLinkedUncertain] = useState(false);
  const [officialUncertain, setOfficialUncertain] = useState(false);
  const [linkedConfirm, setLinkedConfirm] = useState<LinkedConfirmKind>(null);
  const [officialConfirmPause, setOfficialConfirmPause] = useState(false);
  const [nudgeDismissed, setNudgeDismissed] = useState(false);
  const [officialAuthUrl, setOfficialAuthUrl] = useState<string | null>(null);
  const [qrPollCount, setQrPollCount] = useState(1);

  const unsupportedRef = useRef(onUnsupported);
  unsupportedRef.current = onUnsupported;
  const changedRef = useRef(onSettingsChanged);
  changedRef.current = onSettingsChanged;
  const requestSeq = useRef(0);
  const lastInitialMode = useRef<WhatsAppProvider | null | undefined>(initialMode);

  useEffect(() => {
    modeInitializedRef.current = false;
    setSnapshot(null);
    setLoading(true);
    setLoadError(null);
    setActionError(null);
    setStatusNotice(null);
    setLinkedUncertain(false);
    setOfficialUncertain(false);
    setLinkedConfirm(null);
    setOfficialConfirmPause(false);
    setNudgeDismissed(false);
    setOfficialAuthUrl(null);
    setQrPollCount(1);
  }, [installId]);

  useEffect(() => {
    if (lastInitialMode.current !== initialMode) {
      lastInitialMode.current = initialMode;
      if (initialMode === "linked" || initialMode === "official") {
        setFocusedMode(initialMode);
        modeInitializedRef.current = true;
      }
    }
  }, [initialMode]);

  const loadSnapshot = useCallback(
    async (opts?: { silent?: boolean; signal?: AbortSignal }) => {
      const seq = ++requestSeq.current;
      if (!opts?.silent) setLoading(true);
      try {
        const next = await readWhatsAppSettings(installId, opts?.signal);
        if (opts?.signal?.aborted || requestSeq.current !== seq) return null;
        setSnapshot(next);
        setLoadError(null);
        setLoading(false);
        if (!modeInitializedRef.current) {
          modeInitializedRef.current = true;
          setFocusedMode(chooseInitialMode(next, initialMode));
        }
        return next;
      } catch (err) {
        if (opts?.signal?.aborted || requestSeq.current !== seq) return null;
        if (err instanceof DOMException && err.name === "AbortError") return null;
        if (isWhatsAppSettingsUnsupported(err)) {
          markWhatsAppSettingsUnsupported(installId);
          unsupportedRef.current();
          return null;
        }
        if (!opts?.silent) {
          setLoadError(formatError(err, t, "Could not load WhatsApp settings."));
          setLoading(false);
        }
        return null;
      }
    },
    [installId, initialMode, t],
  );

  useEffect(() => {
    const controller = new AbortController();
    void loadSnapshot({ signal: controller.signal });
    return () => controller.abort();
  }, [loadSnapshot]);

  // Visible-tab background polling while pairing/connecting (3s) or connected (15s).
  const linkedStateForPoll =
    snapshot?.linked.available && focusedMode === "linked" && !linkedUncertain
      ? snapshot.linked.state
      : null;

  useEffect(() => {
    if (!linkedStateForPoll) return;
    const fast =
      linkedStateForPoll === "connecting" ||
      linkedStateForPoll === "pairing" ||
      linkedStateForPoll === "disconnected";
    const intervalMs = fast ? 3_000 : linkedStateForPoll === "connected" ? 15_000 : 0;
    if (!intervalMs) return;

    const tick = () => {
      if (typeof document !== "undefined" && document.visibilityState === "hidden") return;
      if (busyAction !== null) return;
      if (linkedStateForPoll === "pairing") setQrPollCount((n) => n + 1);
      void loadSnapshot({ silent: true });
    };

    const timer = window.setInterval(tick, intervalMs);
    const onVisibility = () => {
      if (document.visibilityState === "visible") tick();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [busyAction, linkedStateForPoll, loadSnapshot]);

  const handleSelectMode = async (mode: WhatsAppProvider) => {
    if (!snapshot) return;
    setFocusedMode(mode);
    setActionError(null);
    setStatusNotice(null);
    setLinkedConfirm(null);
    setOfficialConfirmPause(false);

    if (snapshot.viewer_role !== "owner" || snapshot.active_provider === mode) return;

    setBusyAction(`provider:${mode}`);
    try {
      const saved = await saveWhatsAppProvider(installId, mode, snapshot.settings_revision);
      setSnapshot((prev) =>
        prev
          ? {
              ...prev,
              active_provider: saved.active_provider,
              reply_mode: saved.reply_mode,
              settings_revision: saved.settings_revision,
            }
          : prev,
      );
      setStatusNotice(
        saved.active_provider === "official"
          ? t("Active mode switched to Official API.")
          : t("Active mode switched to Linked device."),
      );
      changedRef.current?.();
    } catch (err) {
      if (err instanceof ApiError && err.code === "revision_conflict") {
        await loadSnapshot({ silent: true });
      }
      setActionError(formatError(err, t, "Could not switch WhatsApp mode."));
    } finally {
      setBusyAction(null);
    }
  };

  const handleModeKeyDown = (event: KeyboardEvent<HTMLButtonElement>, current: WhatsAppProvider) => {
    if (event.key !== "ArrowRight" && event.key !== "ArrowDown" && event.key !== "ArrowLeft" && event.key !== "ArrowUp") {
      return;
    }
    event.preventDefault();
    const next: WhatsAppProvider = current === "linked" ? "official" : "linked";
    void handleSelectMode(next);
  };

  const handleLinkedMutation = async (action: "start" | "stop" | "reset", notice?: string) => {
    if (!snapshot || snapshot.viewer_role !== "owner") return;
    setBusyAction(`linked:${action}`);
    setActionError(null);
    setStatusNotice(null);
    try {
      const nextLinked = await runLinkedWhatsAppAction(installId, action);
      setLinkedUncertain(false);
      setLinkedConfirm(null);
      if (action === "reset" || action === "start") setQrPollCount(1);
      setSnapshot((prev) => (prev ? { ...prev, linked: nextLinked } : prev));
      if (notice) setStatusNotice(notice);
      changedRef.current?.();
    } catch (err) {
      if (err instanceof WhatsAppActionUncertainError) {
        setLinkedUncertain(true);
        setLinkedConfirm(null);
      } else {
        setActionError(formatError(err, t, "Could not update Linked WhatsApp device."));
      }
    } finally {
      setBusyAction(null);
    }
  };

  const handleOfficialMutation = async (action: "connect" | "start" | "stop", notice?: string) => {
    if (!snapshot || snapshot.viewer_role !== "owner") return;
    setBusyAction(`official:${action}`);
    setActionError(null);
    setStatusNotice(null);
    setOfficialAuthUrl(null);
    try {
      const result = await runOfficialWhatsAppAction(installId, action);
      setOfficialUncertain(false);
      setOfficialConfirmPause(false);
      if (action === "connect" && result.connect?.url) {
        setOfficialAuthUrl(result.connect.url);
        window.open(result.connect.url, "_blank", "noopener,noreferrer");
        setStatusNotice(t("Opened FavCRM WhatsApp setup in a new tab."));
      } else if (result.official) {
        setSnapshot((prev) => (prev ? { ...prev, official: result.official! } : prev));
        if (notice) setStatusNotice(notice);
      }
      changedRef.current?.();
    } catch (err) {
      if (err instanceof WhatsAppActionUncertainError) {
        setOfficialUncertain(true);
        setOfficialConfirmPause(false);
      } else {
        setActionError(formatError(err, t, "Could not update Official WhatsApp connection."));
      }
    } finally {
      setBusyAction(null);
    }
  };

  const handleRecheck = async () => {
    setBusyAction("recheck");
    setActionError(null);
    const fresh = await loadSnapshot({ silent: true });
    if (fresh) {
      setLinkedUncertain(false);
      setOfficialUncertain(false);
      setStatusNotice(t("Connection status refreshed."));
    }
    setBusyAction(null);
  };

  if (loading && !snapshot) {
    return (
      <main className="wa-settings" aria-labelledby={titleId}>
        <div className="wa-head">
          <div className="wa-head-copy">
            <h1 id={titleId}>{t("WhatsApp connection")}</h1>
            <p className="wa-lead">{t("Loading WhatsApp settings…")}</p>
          </div>
        </div>
        <div className="wa-status-box" role="status">
          <span className="wa-spinner" aria-hidden="true" />
          <div>
            <strong>{t("Checking connection status…")}</strong>
            <p>{t("Reading saved settings and device state from this board.")}</p>
          </div>
        </div>
      </main>
    );
  }

  if (loadError && !snapshot) {
    return (
      <main className="wa-settings" aria-labelledby={titleId}>
        <div className="wa-head">
          <div className="wa-head-copy">
            <h1 id={titleId}>{t("WhatsApp connection")}</h1>
          </div>
        </div>
        <div className="wa-notice fail" role="alert">
          <div className="wa-notice-body">
            <IconWarning size={15} />
            <div>
              <strong>{t("Could not load WhatsApp settings")}</strong>
              <p>{loadError}</p>
            </div>
          </div>
          <Button size="sm" onClick={() => void loadSnapshot()}>
            {t("Retry")}
          </Button>
        </div>
      </main>
    );
  }

  if (!snapshot) return null;

  const isOwner = snapshot.viewer_role === "owner";
  const linkedMeta = linkedBadgeMeta(snapshot.linked, linkedUncertain);
  const officialMeta = officialBadgeMeta(snapshot.official);
  const showIdleNudge = shouldShowLinkedIdleNudge(snapshot, focusedMode, nudgeDismissed || linkedUncertain);
  const busy = busyAction !== null;

  const qrString =
    isOwner &&
    snapshot.linked.available &&
    snapshot.linked.granted &&
    snapshot.linked.state === "pairing"
      ? snapshot.linked.qr
      : null;
  const qrMatrix = qrString ? encodeQrMatrix(qrString) : null;

  const replyModeLabel =
    snapshot.reply_mode === "human-only" ? t("Human-only") : t("Copilot");

  return (
    <main className="wa-settings" aria-labelledby={titleId}>
      <header className="wa-head">
        <div className="wa-head-copy">
          <h1 id={titleId}>{t("WhatsApp connection")}</h1>
          <p className="wa-lead">
            {t("Connect WhatsApp using either a linked device or the Official API.")}
          </p>
        </div>
        {inboxHref ? (
          <Link href={inboxHref} className="btn btn-sm">
            ← {t("Back to Inbox")}
          </Link>
        ) : null}
      </header>

      <div
        className="wa-mode-picker"
        role="radiogroup"
        aria-label={t("Connection mode")}
      >
        <button
          type="button"
          className="wa-mode-tile"
          role="radio"
          aria-checked={focusedMode === "linked"}
          disabled={!isOwner || busy}
          onClick={() => void handleSelectMode("linked")}
          onKeyDown={(e) => handleModeKeyDown(e, "linked")}
        >
          <div className="wa-mode-tile-top">
            <span className="wa-mode-tile-title">
              <IconThreads size={15} />
              <span>{t("Linked device")}</span>
            </span>
            <span className="wa-mode-tile-status">
              <span className={`wa-dot ${linkedMeta.dot}`} aria-hidden="true" />
              <span>{t(linkedMeta.tileLabel)}</span>
            </span>
          </div>
          <p>{t("Scan a QR code from your WhatsApp app")}</p>
        </button>

        <button
          type="button"
          className="wa-mode-tile"
          role="radio"
          aria-checked={focusedMode === "official"}
          disabled={!isOwner || busy}
          onClick={() => void handleSelectMode("official")}
          onKeyDown={(e) => handleModeKeyDown(e, "official")}
        >
          <div className="wa-mode-tile-top">
            <span className="wa-mode-tile-title">
              <IconConnection size={15} />
              <span>{t("Official API")}</span>
            </span>
            <span className="wa-mode-tile-status">
              <span className={`wa-dot ${officialMeta.dot}`} aria-hidden="true" />
              <span>{t(officialMeta.tileLabel)}</span>
            </span>
          </div>
          <p>{t("Meta Business number via FavCRM · Templates & Broadcasts")}</p>
        </button>
      </div>

      {showIdleNudge ? (
        <div className="wa-cost-nudge" role="status">
          <span className="wa-cost-nudge-text">
            <IconInfo size={15} className="wa-cost-nudge-icon" />
            <span>
              {t(
                "Your Linked device is still running in the background. Pause it to avoid idle charges ($0/mo).",
              )}
            </span>
          </span>
          <span className="wa-cost-nudge-actions">
            <Button
              size="sm"
              loading={busyAction === "linked:stop"}
              disabled={busy}
              onClick={() =>
                void handleLinkedMutation("stop", t("Linked device paused ($0/mo)."))
              }
            >
              {t("Pause Linked device")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              onClick={() => setNudgeDismissed(true)}
            >
              {t("Dismiss")}
            </Button>
          </span>
        </div>
      ) : null}

      {actionError ? (
        <div className="wa-notice fail" role="alert">
          <div className="wa-notice-body">
            <IconWarning size={15} />
            <div>
              <strong>{t("Action failed")}</strong>
              <p>{actionError}</p>
            </div>
          </div>
          <Button size="sm" variant="ghost" onClick={() => setActionError(null)}>
            {t("Dismiss")}
          </Button>
        </div>
      ) : null}

      {statusNotice ? (
        <div className="wa-notice" role="status">
          <div className="wa-notice-body">
            <IconCheck size={15} />
            <div>
              <p>{statusNotice}</p>
            </div>
          </div>
        </div>
      ) : null}

      {focusedMode === "linked" ? (
        <section className="wa-conn-card" aria-labelledby={cardHeadingId}>
          <div className="wa-conn-head">
            <div className="wa-conn-identity">
              <span className="wa-section-icon" aria-hidden="true">
                <IconThreads size={18} />
              </span>
              <div className="wa-conn-identity-text">
                <h2 id={cardHeadingId}>{t("Linked device")}</h2>
                <p>{t("Direct WhatsApp Web pairing managed by this workspace.")}</p>
              </div>
            </div>
            <div className="wa-conn-head-right">
              <span className={`wa-badge ${linkedMeta.tone}`}>
                <span className={`wa-dot ${linkedMeta.dot}`} aria-hidden="true" />
                {t(linkedMeta.cardLabel)}
              </span>
              {isOwner &&
              !linkedUncertain &&
              !linkedConfirm &&
              snapshot.linked.available &&
              ["connected", "disconnected", "connecting"].includes(snapshot.linked.state) ? (
                <Button
                  size="sm"
                  variant="ghost"
                  icon={<IconRefresh size={13} />}
                  aria-label={t("Refresh")}
                  disabled={busy}
                  onClick={() => void handleRecheck()}
                />
              ) : null}
            </div>
          </div>

          {linkedUncertain ? (
            <>
              <div className="wa-notice warn" role="alert">
                <div className="wa-notice-body">
                  <IconWarning size={15} />
                  <div>
                    <strong>{t("Couldn’t confirm the latest status")}</strong>
                    <p>
                      {t(
                        "The request timed out before confirming whether anything changed. Check status before trying again.",
                      )}
                    </p>
                  </div>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      variant="primary"
                      icon={<IconRefresh size={13} />}
                      loading={busyAction === "recheck"}
                      disabled={busy}
                      onClick={() => void handleRecheck()}
                    >
                      {t("Check status")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : isOwner && linkedConfirm === "pause" ? (
            <div
              className="wa-inline-confirm warn"
              role="region"
              aria-label={t("Pause Linked device?")}
            >
              <strong>{t("Pause Linked device?")}</strong>
              <p>
                {t(
                  "Stops background hosting ($0/mo). Your WhatsApp phone stays linked—resume anytime without scanning a QR code.",
                )}
              </p>
              <div className="wa-actions-row">
                <Button
                  variant="primary"
                  loading={busyAction === "linked:stop"}
                  disabled={busy}
                  onClick={() =>
                    void handleLinkedMutation("stop", t("Linked device paused ($0/mo)."))
                  }
                >
                  {t("Pause now")}
                </Button>
                <Button
                  variant="ghost"
                  disabled={busy}
                  onClick={() => setLinkedConfirm(null)}
                >
                  {t("Cancel")}
                </Button>
              </div>
            </div>
          ) : isOwner && linkedConfirm === "disconnect" ? (
            <div
              className="wa-inline-confirm danger"
              role="region"
              aria-label={t("Disconnect and unlink this device?")}
            >
              <strong>{t("Disconnect and unlink this device?")}</strong>
              <p>
                {t(
                  "Signs out this WhatsApp session and clears saved device credentials. Reconnecting later requires scanning a new QR code.",
                )}
              </p>
              <div className="wa-actions-row">
                <Button
                  variant="danger"
                  loading={busyAction === "linked:reset"}
                  disabled={busy}
                  onClick={() =>
                    void handleLinkedMutation("reset", t("Session cleared. Scan a new QR code to re-link."))
                  }
                >
                  {t("Disconnect & unlink")}
                </Button>
                <Button
                  variant="ghost"
                  disabled={busy}
                  onClick={() => setLinkedConfirm(null)}
                >
                  {t("Cancel")}
                </Button>
              </div>
            </div>
          ) : isOwner && linkedConfirm === "relink" ? (
            <div
              className="wa-inline-confirm warn"
              role="region"
              aria-label={t("Start fresh QR pairing?")}
            >
              <strong>{t("Start fresh QR pairing?")}</strong>
              <p>
                {t(
                  "Clears the expired session and generates a new QR code to scan with your phone.",
                )}
              </p>
              <div className="wa-actions-row">
                <Button
                  variant="primary"
                  loading={busyAction === "linked:reset"}
                  disabled={busy}
                  onClick={() => void handleLinkedMutation("reset")}
                >
                  {t("Generate new QR")}
                </Button>
                <Button
                  variant="ghost"
                  disabled={busy}
                  onClick={() => setLinkedConfirm(null)}
                >
                  {t("Cancel")}
                </Button>
              </div>
            </div>
          ) : !snapshot.linked.available ? (
            <>
              <div className="wa-notice warn" role="alert">
                <div className="wa-notice-body">
                  <IconWarning size={15} />
                  <div>
                    <strong>{t("Linked WhatsApp service is unavailable")}</strong>
                    <p>{t("Try checking status again in a moment.")}</p>
                  </div>
                </div>
              </div>
              <div className="wa-card-footer">
                <div className="wa-card-footer-left">
                  <Button
                    size="sm"
                    loading={busyAction === "recheck"}
                    disabled={busy}
                    onClick={() => void handleRecheck()}
                  >
                    {t("Retry")}
                  </Button>
                </div>
              </div>
            </>
          ) : !snapshot.linked.granted || snapshot.linked.state === "not_granted" ? (
            <>
              <div className="wa-notice">
                <div className="wa-notice-body">
                  <IconLock size={15} />
                  <div>
                    <strong>{t("Linked device not enabled")}</strong>
                    <p>
                      {t(
                        "Linked device pairing is not enabled for this workspace. Use the Official API or contact your administrator.",
                      )}
                    </p>
                  </div>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button disabled={busy} onClick={() => void handleSelectMode("official")}>
                      {t("Switch to Official API")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : snapshot.linked.state === "idle" ? (
            <>
              <div className="wa-status-box">
                <IconThreads size={16} />
                <div>
                  <strong>{t("No device linked yet")}</strong>
                  <p>
                    {t(
                      "Link your WhatsApp phone with a QR code to handle Inbox messages directly in Cadence.",
                    )}
                  </p>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      variant="primary"
                      loading={busyAction === "linked:start"}
                      disabled={busy}
                      onClick={() => void handleLinkedMutation("start")}
                    >
                      {t("Link a device")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : snapshot.linked.state === "connecting" ? (
            <>
              <div className="wa-status-box" role="status">
                <span className="wa-spinner" aria-hidden="true" />
                <div>
                  <strong>{t("Starting WhatsApp session…")}</strong>
                  <p>{t("Usually takes 10–20 seconds.")}</p>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-right">
                    <Button
                      variant="ghost"
                      disabled={busy}
                      onClick={() => void handleLinkedMutation("stop")}
                    >
                      {t("Cancel")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : snapshot.linked.state === "pairing" ? (
            !isOwner ? (
              <div className="wa-status-box">
                <IconLock size={15} />
                <div>
                  <strong>{t("Waiting for owner to scan QR")}</strong>
                  <p>{t("Only workspace owners can pair devices or change connection settings.")}</p>
                </div>
              </div>
            ) : !qrMatrix ? (
              <>
                <div className="wa-notice warn">
                  <div className="wa-notice-body">
                    <IconWarning size={15} />
                    <div>
                      <strong>{t("QR code is preparing or expired")}</strong>
                      <p>{t("Refresh or generate a new QR code to continue linking your phone.")}</p>
                    </div>
                  </div>
                </div>
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      variant="primary"
                      icon={<IconRefresh size={13} />}
                      loading={busyAction === "recheck"}
                      disabled={busy}
                      onClick={() => void handleRecheck()}
                    >
                      {t("Refresh")}
                    </Button>
                    <Button
                      loading={busyAction === "linked:reset"}
                      disabled={busy}
                      onClick={() => void handleLinkedMutation("reset")}
                    >
                      {t("Generate new QR")}
                    </Button>
                  </div>
                  <div className="wa-card-footer-right">
                    <Button
                      variant="ghost"
                      disabled={busy}
                      onClick={() => void handleLinkedMutation("stop")}
                    >
                      {t("Cancel")}
                    </Button>
                  </div>
                </div>
              </>
            ) : (
              <>
                <div className="wa-qr-panel">
                  <div className="wa-qr-box">
                    <div className="wa-qr-wrap">
                      <svg
                        viewBox={`-4 -4 ${qrMatrix.size + 8} ${qrMatrix.size + 8}`}
                        width={162}
                        height={162}
                        role="img"
                        aria-label={t("WhatsApp pairing QR code")}
                        shapeRendering="crispEdges"
                      >
                        <rect
                          x={-4}
                          y={-4}
                          width={qrMatrix.size + 8}
                          height={qrMatrix.size + 8}
                          fill="#ffffff"
                        />
                        <path d={qrSvgPath(qrMatrix)} fill="#111318" />
                      </svg>
                    </div>
                    <div className="wa-qr-meta">
                      <span className="wa-qr-live">
                        <span className="wa-pulse" aria-hidden="true" />
                        <span>
                          {t("Auto-refreshing · #{n}").replace("{n}", () => String(qrPollCount))}
                        </span>
                      </span>
                      <Button
                        size="sm"
                        variant="ghost"
                        icon={<IconRefresh size={12} />}
                        disabled={busy}
                        onClick={() => void handleRecheck()}
                      >
                        {t("Refresh")}
                      </Button>
                    </div>
                  </div>
                  <div className="wa-qr-steps">
                    <strong>{t("Scan with WhatsApp")}</strong>
                    <ol>
                      <li>{t("Open WhatsApp on your phone.")}</li>
                      <li>{t("Go to Settings → Linked Devices → Link a Device.")}</li>
                      <li>{t("Point your camera at this QR code.")}</li>
                    </ol>
                  </div>
                </div>
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      size="sm"
                      disabled={busy}
                      onClick={() => setLinkedConfirm("relink")}
                    >
                      {t("Generate new QR")}
                    </Button>
                  </div>
                  <div className="wa-card-footer-right">
                    <Button
                      variant="ghost"
                      disabled={busy}
                      onClick={() => void handleLinkedMutation("stop")}
                    >
                      {t("Cancel")}
                    </Button>
                  </div>
                </div>
              </>
            )
          ) : snapshot.linked.state === "stopped" ? (
            <>
              <div className="wa-notice warn">
                <div className="wa-notice-body">
                  <IconInfo size={15} />
                  <div>
                    <strong>{t("Linked device is paused")}</strong>
                    <p>
                      {t(
                        "Hosting is paused ($0/mo). Your WhatsApp phone is still linked—resume anytime without scanning a QR code.",
                      )}
                    </p>
                  </div>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      variant="primary"
                      loading={busyAction === "linked:start"}
                      disabled={busy}
                      onClick={() =>
                        void handleLinkedMutation("start", t("Resuming Linked WhatsApp session…"))
                      }
                    >
                      {t("Resume")}
                    </Button>
                  </div>
                  <div className="wa-card-footer-right">
                    <Button
                      variant="danger"
                      disabled={busy}
                      onClick={() => setLinkedConfirm("disconnect")}
                    >
                      {t("Disconnect device")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : snapshot.linked.state === "disconnected" ? (
            <>
              <div className="wa-status-box" role="status">
                <span className="wa-spinner" aria-hidden="true" />
                <div>
                  <strong>{t("Reconnecting…")}</strong>
                  <p>{t("Connection interrupted. Reconnecting automatically…")}</p>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button disabled={busy} onClick={() => setLinkedConfirm("pause")}>
                      {t("Pause")}
                    </Button>
                  </div>
                  <div className="wa-card-footer-right">
                    <Button
                      variant="danger"
                      disabled={busy}
                      onClick={() => setLinkedConfirm("disconnect")}
                    >
                      {t("Disconnect device")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : snapshot.linked.state === "reauthorization_required" ? (
            <>
              <div className="wa-notice fail">
                <div className="wa-notice-body">
                  <IconWarning size={15} />
                  <div>
                    <strong>{t("Session expired")}</strong>
                    <p>
                      {t(
                        "This device was logged out from your phone. Scan a new QR code to reconnect.",
                      )}
                    </p>
                  </div>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      variant="primary"
                      icon={<IconRefresh size={13} />}
                      disabled={busy}
                      onClick={() => setLinkedConfirm("relink")}
                    >
                      {t("Scan new QR")}
                    </Button>
                  </div>
                  <div className="wa-card-footer-right">
                    <Button
                      variant="ghost"
                      disabled={busy}
                      onClick={() => void handleLinkedMutation("stop")}
                    >
                      {t("Pause")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : (
            <>
              <div className="wa-rows">
                <div className="wa-row">
                  <span className="wa-row-label">{t("Status")}</span>
                  <span className="wa-row-val">{t("Active · Messages syncing")}</span>
                </div>
                <div className="wa-row">
                  <span className="wa-row-label">{t("Hosting")}</span>
                  <span className="wa-row-val">{t("Running · Pause anytime for $0/mo")}</span>
                </div>
                <div className="wa-row">
                  <span className="wa-row-label">{t("Reply mode")}</span>
                  <span className="wa-row-val">{replyModeLabel}</span>
                </div>
                <div className="wa-row">
                  <span className="wa-row-label">{t("Features")}</span>
                  <span className="wa-row-val">{t("Inbox & direct replies")}</span>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button disabled={busy} onClick={() => setLinkedConfirm("pause")}>
                      {t("Pause")}
                    </Button>
                  </div>
                  <div className="wa-card-footer-right">
                    <Button
                      variant="danger"
                      disabled={busy}
                      onClick={() => setLinkedConfirm("disconnect")}
                    >
                      {t("Disconnect device")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          )}
        </section>
      ) : (
        <section className="wa-conn-card" aria-labelledby={cardHeadingId}>
          <div className="wa-conn-head">
            <div className="wa-conn-identity">
              <span className="wa-section-icon" aria-hidden="true">
                <IconConnection size={18} />
              </span>
              <div className="wa-conn-identity-text">
                <h2 id={cardHeadingId}>
                  {snapshot.official.available && snapshot.official.display_phone_number
                    ? snapshot.official.display_phone_number
                    : t("Official API")}
                </h2>
                <p>
                  {snapshot.official.available && snapshot.official.verified_name
                    ? snapshot.official.verified_name
                    : t("Meta Business Cloud API via FavCRM")}
                </p>
              </div>
            </div>
            <div className="wa-conn-head-right">
              <span className={`wa-badge ${officialMeta.tone}`}>
                <span className={`wa-dot ${officialMeta.dot}`} aria-hidden="true" />
                {t(officialMeta.cardLabel)}
              </span>
              {isOwner && !officialUncertain && !officialConfirmPause ? (
                <Button
                  size="sm"
                  variant="ghost"
                  icon={<IconRefresh size={13} />}
                  aria-label={t("Refresh")}
                  disabled={busy}
                  onClick={() => void handleRecheck()}
                />
              ) : null}
            </div>
          </div>

          {officialUncertain ? (
            <>
              <div className="wa-notice warn" role="alert">
                <div className="wa-notice-body">
                  <IconWarning size={15} />
                  <div>
                    <strong>{t("Couldn’t confirm the latest status")}</strong>
                    <p>
                      {t(
                        "The request timed out before confirming whether anything changed. Check status before trying again.",
                      )}
                    </p>
                  </div>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      variant="primary"
                      icon={<IconRefresh size={13} />}
                      loading={busyAction === "recheck"}
                      disabled={busy}
                      onClick={() => void handleRecheck()}
                    >
                      {t("Check status")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : isOwner && officialConfirmPause ? (
            <div
              className="wa-inline-confirm warn"
              role="region"
              aria-label={t("Pause Official API?")}
            >
              <strong>{t("Pause Official API?")}</strong>
              <p>
                {t(
                  "Pauses Official API messaging for this workspace without disconnecting Meta or FavCRM.",
                )}
              </p>
              <div className="wa-actions-row">
                <Button
                  variant="primary"
                  loading={busyAction === "official:stop"}
                  disabled={busy}
                  onClick={() => void handleOfficialMutation("stop", t("Official API paused."))}
                >
                  {t("Pause now")}
                </Button>
                <Button
                  variant="ghost"
                  disabled={busy}
                  onClick={() => setOfficialConfirmPause(false)}
                >
                  {t("Cancel")}
                </Button>
              </div>
            </div>
          ) : !snapshot.official.available ? (
            <>
              <div className="wa-notice warn">
                <div className="wa-notice-body">
                  <IconWarning size={15} />
                  <div>
                    <strong>{t("Official WhatsApp status unavailable")}</strong>
                    <p>{t("Try checking status again in a moment.")}</p>
                  </div>
                </div>
              </div>
              <div className="wa-card-footer">
                <div className="wa-card-footer-left">
                  <Button
                    size="sm"
                    loading={busyAction === "recheck"}
                    disabled={busy}
                    onClick={() => void handleRecheck()}
                  >
                    {t("Retry")}
                  </Button>
                </div>
              </div>
            </>
          ) : snapshot.official.paused ? (
            <>
              <div className="wa-notice warn">
                <div className="wa-notice-body">
                  <IconInfo size={15} />
                  <div>
                    <strong>{t("Official API is paused")}</strong>
                    <p>
                      {t(
                        "Workspace pause overlay is active. Your FavCRM and Meta setup remains untouched—resume anytime.",
                      )}
                    </p>
                  </div>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      variant="primary"
                      loading={busyAction === "official:start"}
                      disabled={busy}
                      onClick={() =>
                        void handleOfficialMutation("start", t("Official API resumed."))
                      }
                    >
                      {t("Resume")}
                    </Button>
                  </div>
                  <div className="wa-card-footer-right">
                    <Button
                      loading={busyAction === "official:connect"}
                      disabled={busy}
                      onClick={() => void handleOfficialMutation("connect")}
                    >
                      {t("Manage in FavCRM")} ↗
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : snapshot.official.connected ? (
            <>
              <div className="wa-rows">
                <div className="wa-row">
                  <span className="wa-row-label">{t("Status")}</span>
                  <span className="wa-row-val">{t("Active · Messages & webhooks live")}</span>
                </div>
                {snapshot.official.company_name ? (
                  <div className="wa-row">
                    <span className="wa-row-label">{t("Business")}</span>
                    <span className="wa-row-val">{snapshot.official.company_name}</span>
                  </div>
                ) : null}
                {snapshot.official.display_phone_number ? (
                  <div className="wa-row">
                    <span className="wa-row-label">{t("Phone number")}</span>
                    <span className="wa-row-val wa-mono">
                      {snapshot.official.display_phone_number}
                    </span>
                  </div>
                ) : null}
                {snapshot.official.webhook ? (
                  <div className="wa-row">
                    <span className="wa-row-label">{t("Webhook")}</span>
                    <span className="wa-row-val">
                      {snapshot.official.webhook.subscribed
                        ? t("Subscribed")
                        : t("Pending verification")}
                    </span>
                  </div>
                ) : null}
                <div className="wa-row">
                  <span className="wa-row-label">{t("Reply mode")}</span>
                  <span className="wa-row-val">{replyModeLabel}</span>
                </div>
                <div className="wa-row">
                  <span className="wa-row-label">{t("Features")}</span>
                  <span className="wa-row-val">
                    {t("Inbox, Templates, Broadcasts & AI Agent")}
                  </span>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button disabled={busy} onClick={() => setOfficialConfirmPause(true)}>
                      {t("Pause")}
                    </Button>
                  </div>
                  <div className="wa-card-footer-right">
                    <Button
                      loading={busyAction === "official:connect"}
                      disabled={busy}
                      onClick={() => void handleOfficialMutation("connect")}
                    >
                      {t("Manage in FavCRM")} ↗
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          ) : (
            <>
              <div className="wa-status-box">
                <IconConnection size={16} />
                <div>
                  <strong>{t("Official API not connected")}</strong>
                  <p>
                    {t(
                      "Connect a Meta Business Cloud number through FavCRM to unlock Templates, Broadcasts, and AI Agent.",
                    )}
                  </p>
                </div>
              </div>
              {isOwner ? (
                <div className="wa-card-footer">
                  <div className="wa-card-footer-left">
                    <Button
                      variant="primary"
                      loading={busyAction === "official:connect"}
                      disabled={busy}
                      onClick={() => void handleOfficialMutation("connect")}
                    >
                      {t("Connect via FavCRM")} ↗
                    </Button>
                  </div>
                </div>
              ) : null}
            </>
          )}

          {officialAuthUrl ? (
            <div className="wa-notice" role="status">
              <div className="wa-notice-body">
                <IconInfo size={15} />
                <div>
                  <strong>{t("FavCRM authorization link ready")}</strong>
                  <p>
                    <a href={officialAuthUrl} target="_blank" rel="noopener noreferrer">
                      {t("Continue setup in FavCRM")} ↗
                    </a>
                  </p>
                </div>
              </div>
            </div>
          ) : null}
        </section>
      )}

      <footer className="wa-privacy-foot">
        <IconLock size={13} />
        <span>
          {isOwner
            ? t("Pairing codes and connection controls are private to workspace owners.")
            : t("Only workspace owners can pair devices or change connection settings.")}
        </span>
      </footer>
    </main>
  );
}
