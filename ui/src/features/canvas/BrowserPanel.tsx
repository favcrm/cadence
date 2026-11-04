import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslate } from "../../lib/locale";
import { IconClose } from "../../ui/icons";
import { useCanvas } from "./canvasStore";
import { EmptyState, ErrorNotice, Notice } from "../app-shell/shared/States";
import { viewBrowser } from "./browserClient";
import {
  cancelStop,
  confirmStop,
  openBrowser,
  refreshBrowser,
  requestStop,
  tabVisible,
  useBrowserTab,
  type BrowserTabState,
} from "./browserStore";

/** How long a live view may take to load before the panel says so. */
const FRAME_WAIT_MS = 20000;
const LONG_INSTRUCTIONS = 180;

export function GlobeIcon({ size = 14 }: { size?: number }) {
  return (
    <svg viewBox="0 0 24 24" width={size} height={size} fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <circle cx="12" cy="12" r="9" />
      <path d="M3 12h18M12 3c3 3.2 3 14.8 0 18M12 3c-3 3.2-3 14.8 0 18" />
    </svg>
  );
}

type Tone = "accent" | "warn" | "muted" | "fail";
export interface BrowserStatus {
  text: string;
  tone: Tone;
  pulse: boolean;
}

/** The one word for the tab, header and announcements. */
export function browserStatus(s: BrowserTabState, t: (english: string) => string): BrowserStatus {
  if (s.stop === "stopping") return { text: t("Stopping…"), tone: "warn", pulse: true };
  if (s.loadError) return { text: t("Can't load"), tone: "fail", pulse: false };
  switch (s.session?.state) {
    case "active":
      return { text: t("Reading"), tone: "accent", pulse: true };
    case "handoff":
      return { text: t("Needs you"), tone: "warn", pulse: true };
    case "ended":
      return { text: t("Ended"), tone: "muted", pulse: false };
    default:
      return { text: t("No session"), tone: "muted", pulse: false };
  }
}

const TONE: Record<Tone, string> = {
  accent: "var(--color-accent)",
  warn: "var(--color-warn)",
  muted: "var(--color-ink-500)",
  fail: "var(--color-fail)",
};

/** The tab's label: icon, name, a state dot, and the state in words for a
 *  screen reader. */
export function BrowserTabLabel() {
  const t = useTranslate();
  const b = useBrowserTab();
  const st = browserStatus(b, t);
  return (
    <>
      <GlobeIcon />
      <span>{t("Browser")}</span>
      <span className="bz-dot" style={{ "--c": TONE[st.tone] } as React.CSSProperties} data-pulse={st.pulse || undefined} aria-hidden />
      <span className="sr-only">{st.text}</span>
    </>
  );
}

/** Split an address for display only: scheme, real host, the rest. The host is
 *  what the URL parser reports (punycode stays punycode); nothing is matched
 *  against it. An address that does not parse is shown whole. */
function addressParts(address: string): { scheme: string; host: string; rest: string } | null {
  try {
    const u = new URL(address);
    return { scheme: `${u.protocol}//`, host: u.host, rest: `${u.pathname}${u.search}` };
  } catch {
    return null;
  }
}

function AddressRow({ b }: { b: BrowserTabState }) {
  const t = useTranslate();
  const live = b.session && (b.session.state === "active" || b.session.state === "handoff");
  const parts = live && b.session?.url ? addressParts(b.session.url) : null;
  const empty = live && b.session?.url ? b.session.url : t("No page open");
  return (
    <div className="bz-addr" aria-label={t("Page address")}>
      <div className="bz-url" title={live ? (b.session?.url ?? undefined) : undefined} data-empty={parts ? undefined : true}>
        <GlobeIcon size={12} />
        {parts ? (
          <>
            <span className="bz-scheme">{parts.scheme}</span>
            <b className="bz-host">{parts.host}</b>
            <span className="bz-rest">{parts.rest}</span>
          </>
        ) : (
          <span>{empty}</span>
        )}
      </div>
      {live && b.session?.signedIn ? (
        <span className="bz-signed" title={t("Signed in. The assistant only reads the current page.")}>
          {t("Signed in")}
          <span className="sr-only">. {t("The assistant only reads the current page.")}</span>
        </span>
      ) : null}
    </div>
  );
}

/** The assistant's words, plain text only: a text node, never HTML, never a
 *  link, framed and attributed because a page can shape what it writes. */
function Says({ b, compact }: { b: BrowserTabState; compact: boolean }) {
  const t = useTranslate();
  const [clamp, setClamp] = useState(true);
  const text = b.session?.instructions ?? "";
  const long = text.length > LONG_INSTRUCTIONS;
  const parts = b.session?.url ? addressParts(b.session.url) : null;
  return (
    <section className="bz-says" aria-labelledby="bz-says-h">
      <div className="bz-says-h" id="bz-says-h">{t("Your assistant is asking you to take over")}</div>
      <p className="bz-says-t" id="bz-says-t" data-clamp={long && clamp ? (compact ? "compact" : "full") : undefined}>{text}</p>
      {long ? (
        <button type="button" className="btn btn-sm btn-ghost bz-more" aria-expanded={!clamp} aria-controls="bz-says-t" onClick={() => setClamp((c) => !c)}>
          {clamp ? t("Show all") : t("Show less")}
        </button>
      ) : null}
      <p className="bz-says-f">{t("Written by the assistant from what it read. It can be wrong or shaped by the page. Check the address before you type anything.")}</p>
      {!compact && parts ? (
        <p className="bz-says-addr">
          <small>{t("You will be on this page")}</small>
          <code>
            <span>{parts.scheme}</span>
            <b>{parts.host}</b>
            <span>{parts.rest}</span>
          </code>
        </p>
      ) : null}
    </section>
  );
}

type Viewer =
  | { phase: "idle" }
  | { phase: "connecting" }
  | { phase: "loading" | "open"; url: string }
  | { phase: "conflict" | "error" };

/**
 * The Browser tab's panel. Rendered while the tab exists and merely hidden
 * when another tab is active, so a live view in progress survives a tab
 * switch. The viewer URL lives only in this component's state, from the
 * person's click until the frame is torn down, and every teardown resets the
 * viewer to idle so the next open fetches a fresh URL.
 */
export default function BrowserPanel({ conversation, active, sheet, expanded, onExpand, onClose }: {
  conversation: string;
  active: boolean;
  sheet: boolean;
  expanded: boolean;
  onExpand: () => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const b = useBrowserTab();
  const [viewer, setViewer] = useState<Viewer>({ phase: "idle" });
  const [now, setNow] = useState(() => Date.now());
  const [note, setNote] = useState("");
  const attempt = useRef(0);
  const dockRef = useRef<HTMLDivElement | null>(null);
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const root = useRef<HTMLDivElement | null>(null);
  const hadFocus = useRef(false);
  const st = browserStatus(b, t);
  const phase = b.session?.state ?? "none";
  const live = phase === "active" || phase === "handoff";
  const stopping = b.stop === "stopping";
  const viewing = viewer.phase === "loading" || viewer.phase === "open";
  // A takeover over, or any other phase, tears the frame down. A stop in
  // flight does not: the view stays until the stop has succeeded.
  const showFrame = viewing && phase === "handoff";

  useEffect(() => {
    if (phase !== "handoff" && viewer.phase !== "idle") {
      attempt.current++;
      setViewer({ phase: "idle" });
    }
  }, [phase, viewer.phase]);

  // The frame is refused by the board's own CSP: say so now. (A frame refused
  // by the provider's X-Frame-Options or frame-ancestors, or one that answers
  // with an error page, still fires `load`: nothing here can tell, which is why
  // the wording never claims the person is in control.)
  useEffect(() => {
    if (viewer.phase !== "loading") return;
    const onViolation = (e: Event) => {
      if ((e as Event & { effectiveDirective?: string }).effectiveDirective === "frame-src") setViewer({ phase: "error" });
    };
    document.addEventListener("securitypolicyviolation", onViolation);
    // A frame that never answers is reported, not left spinning.
    const id = setTimeout(() => setViewer({ phase: "error" }), FRAME_WAIT_MS);
    return () => {
      document.removeEventListener("securitypolicyviolation", onViolation);
      clearTimeout(id);
    };
  }, [viewer.phase]);

  useEffect(() => {
    if (!stopping) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [stopping]);

  // Spoken state changes, once each.
  const spoken = `${phase}|${b.stop}|${b.loadError}|${b.handedBack}|${viewer.phase}`;
  useEffect(() => {
    const words =
      b.stop === "failed" ? t("Couldn't stop the session. It may still be running.")
      : stopping ? t("Stopping the session")
      : b.handedBack ? t("Handed back. The assistant is reading again.")
      : b.loadError ? t("Can't load the browser state")
      : viewer.phase === "error" ? t("Couldn't open the live view.")
      : `${t("Browser")}: ${st.text}`;
    setNote(words);
  }, [spoken]);

  // Keep the page clear of the floating dock, whatever its height.
  useLayoutEffect(() => {
    const dock = dockRef.current;
    const body = bodyRef.current;
    if (body) body.style.paddingBottom = dock ? `${dock.offsetHeight + 56}px` : "";
  });

  // A control that held focus and went away (Stop, Try again, the frame) must
  // not drop focus to the page: it goes to the retry control on a failure and
  // to the tab otherwise. A deliberate click elsewhere clears the claim.
  useEffect(() => {
    if (!active || !hadFocus.current || document.activeElement !== document.body) return;
    const target =
      b.stop === "failed" ? '[data-bz-act="go"], [data-bz-act="stop"]'
      : b.loadError ? ".serror button"
      : viewer.phase === "error" ? '[data-bz-act="view"]'
      : null;
    const next = target ? root.current?.querySelector<HTMLElement>(target) : document.getElementById("bz-tab");
    next?.focus();
  });

  const focusSoon = (selector: string) => requestAnimationFrame(() => root.current?.querySelector<HTMLElement>(selector)?.focus());

  const openView = async () => {
    const mine = ++attempt.current;
    setViewer({ phase: "connecting" });
    try {
      const r = await viewBrowser(conversation);
      if (mine !== attempt.current) return;
      if (r.kind === "not_in_handoff") {
        setViewer({ phase: "conflict" });
        void refreshBrowser();
        return;
      }
      setViewer({ phase: "loading", url: r.url });
      focusSoon("[data-bz-frame]");
    } catch {
      if (mine === attempt.current) setViewer({ phase: "error" });
    }
  };

  const ask = () => {
    requestStop();
    focusSoon('[data-bz-act="keep"]');
  };
  const keep = () => {
    cancelStop();
    focusSoon('[data-bz-act="stop"]');
  };
  const stop = () => {
    void confirmStop();
    focusSoon("[data-bz-dock]");
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    // Esc cancels a pending stop first; only then does it reach the drawer.
    if (e.key === "Escape" && b.stop === "confirm") {
      e.preventDefault();
      keep();
    }
  };

  const stopBtn = (label: string) => (
    <button type="button" className="btn btn-danger" data-bz-act="stop" onClick={b.stop === "failed" ? stop : ask}>{label}</button>
  );

  const sessionBody = (): React.ReactNode => {
    if (phase === "none") {
      return <EmptyState title={t("No browser session")}>{t("Ask your assistant to look something up on the web. A session it opens appears here and keeps running if you close this panel.")}</EmptyState>;
    }
    if (phase === "ended") {
      return (
        <EmptyState title={t("Session ended")}>
          {t("The assistant is no longer browsing. Ask for a new one if it needs to read more pages.")}
          {b.session?.url ? <span className="bz-tag">{t("Last address")}: {b.session.url}</span> : null}
        </EmptyState>
      );
    }
    if (phase === "active") {
      return (
        <>
          <EmptyState title={stopping ? t("Closing the session") : t("Your assistant is reading this page")}>
            {stopping
              ? t("The assistant has stopped reading while the browser shuts down.")
              : t("It reads pages on its own and doesn't click or type. Live view isn't offered outside a takeover, so this shows the address and state only.")}
            <span className="cv-bar bz-bar" aria-hidden><i className="cv-bar-run" /></span>
          </EmptyState>
          {b.handedBack ? <div role="status"><Notice>{t("Handed back. The assistant is reading again.")}</Notice></div> : null}
          {b.session?.signedIn ? <Notice>{t("You signed in on this session. The assistant will only read the current page, not move to others.")}</Notice> : null}
        </>
      );
    }
    if (showFrame && (viewer.phase === "loading" || viewer.phase === "open")) {
      return (
        <>
          <Says b={b} compact />
          <div className="bz-viewer" data-phase={viewer.phase}>
            <div className="bz-viewer-bar">
              <span>{addressParts(b.session?.url ?? "")?.host ?? ""}</span>
              <b>{viewer.phase === "open" ? t("Live view") : t("Connecting…")}</b>
            </div>
            <iframe
              data-bz-frame
              title={t("Live view")}
              src={viewer.url}
              // The provider's page needs its own scripts and forms; it is a
              // different origin (the client refuses the board's own), so none
              // of this reaches the board.
              sandbox="allow-scripts allow-same-origin allow-forms"
              referrerPolicy="no-referrer"
              onLoad={() => setViewer((v) => (v.phase === "loading" ? { phase: "open", url: v.url } : v))}
            />
          </div>
        </>
      );
    }
    return (
      <>
        <Says b={b} compact={false} />
        {viewer.phase === "connecting" ? <div role="status"><Notice>{t("Opening live view…")}</Notice></div> : null}
        {viewer.phase === "conflict" ? <div role="alert"><Notice>{t("Your assistant isn't asking for a takeover any more, so there is nothing to open. The panel will refresh.")}</Notice></div> : null}
        {viewer.phase === "error" ? <ErrorNotice>{t("Couldn't open the live view. Check your connection and try again. If it keeps failing, the live view may not be allowed on this board yet.")}</ErrorNotice> : null}
      </>
    );
  };

  const body = (
    <>
      {b.loadError ? (
        <ErrorNotice onRetry={() => void refreshBrowser()} retryLabel={t("Try again")}>
          {t("Couldn't load the browser state. Your session, if there is one, is unaffected.")}
        </ErrorNotice>
      ) : null}
      {sessionBody()}
    </>
  );

  const elapsed = b.stopStartedAt === null ? 0 : Math.max(0, Math.round((now - b.stopStartedAt) / 1000));
  let dock: React.ReactNode = null;
  if (b.stop === "confirm") {
    const lost = b.session?.signedIn ? ` ${t("You are signed in here, and that is lost.")}` : "";
    const takeover = b.session?.state === "handoff" ? ` ${t("The takeover ends.")}` : "";
    dock = (
      <>
        <p role="alert">{t("Stop the session?")}{lost}{takeover} {t("The assistant stops reading pages.")}</p>
        <div className="bz-dock-row">
          <button type="button" className="btn btn-danger" data-bz-act="go" onClick={stop}>{t("Stop session")}</button>
          <button type="button" className="btn" data-bz-act="keep" onClick={keep}>{t("Keep it running")}</button>
        </div>
      </>
    );
  } else if (stopping) {
    dock = (
      <>
        <div className="bz-dock-row">
          <button type="button" className="btn btn-danger" disabled aria-disabled="true" aria-busy="true">
            <span className="btn-spin" aria-hidden />
            <span className="btn-label">{t("Stopping…")}</span>
          </button>
        </div>
        <p>{t("Waiting for the browser to close. A page that isn't responding can take a while.")} <span className="bz-elapsed">{elapsed} s</span></p>
        <p className="bz-hint">{t("You can leave this tab; it keeps stopping. Nothing else is needed from you.")}</p>
      </>
    );
  } else if (live) {
    // The failure sits above the phase's normal actions, whatever they are.
    const failed = b.stop === "failed"
      ? <p className="bz-alert" role="alert">{t("Couldn't stop the session. It may still be running.")}</p>
      : null;
    if (phase === "handoff") {
      const wait = viewer.phase === "connecting";
      dock = (
        <>
          {failed}
          <div className="bz-dock-row">
            <button type="button" className={viewing ? "btn" : "btn btn-primary"} data-bz-act="view" disabled={wait} aria-disabled={wait} onClick={() => void openView()}>
              {wait ? <span className="btn-spin" aria-hidden /> : null}
              <span className="btn-label">{viewing || viewer.phase === "error" ? t("Open live view again") : t("Open live view")}</span>
            </button>
            {stopBtn(b.stop === "failed" ? t("Try stopping again") : t("Stop session"))}
          </div>
          {viewing ? <p className="bz-hint">{t("When you are finished, use Done in the live view. Your assistant carries on reading. Stop ends the whole session.")}</p> : null}
        </>
      );
    } else {
      dock = (
        <>
          {failed}
          <div className="bz-dock-row">{stopBtn(b.stop === "failed" ? t("Try stopping again") : t("Stop session"))}</div>
          <p className="bz-hint">{t("Need to sign in yourself? Ask your assistant to hand over. You can't start a takeover from here.")}</p>
        </>
      );
    }
  }

  return (
    <div
      className="bz-panel"
      role="tabpanel"
      id="bz-panel"
      aria-labelledby="bz-tab"
      hidden={!active}
      ref={root}
      onKeyDown={onKeyDown}
      onFocusCapture={() => { hadFocus.current = true; }}
      onBlurCapture={(e) => { if (e.relatedTarget === null && (e.target as Element).isConnected) hadFocus.current = false; }}
    >
      <header className="cv-head">
        {sheet ? (
          <button type="button" className="cv-iconbtn cv-back" onClick={onClose} aria-label={t("Back to chat")}>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
        ) : null}
        <span className="arttype"><GlobeIcon size={16} /></span>
        <span className="cv-title">
          <b>{t("Browser")}</b>
          <span>
            <span className="bz-badge" style={{ "--c": TONE[st.tone] } as React.CSSProperties}>{st.text}</span>
            <span className="bz-remind">
              {t("session")}
              {live && b.stop === "idle" ? ` · ${t("keeps running if you close this")}` : ""}
            </span>
          </span>
        </span>
        {!sheet ? (
          <>
            <button type="button" className="cv-iconbtn" onClick={onExpand} aria-pressed={expanded} aria-label={t("Expand canvas")} title={t("Expand canvas")}>
              <svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
                {expanded ? <path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" /> : <path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />}
              </svg>
            </button>
            <button type="button" className="cv-iconbtn" onClick={onClose} aria-label={t("Close canvas")}>
              <IconClose size={12} />
            </button>
          </>
        ) : null}
      </header>
      <AddressRow b={b} />
      <div className="cv-body bz-body" ref={bodyRef} data-canvas-body>{body}</div>
      {dock ? <div className="bz-dock" ref={dockRef} data-bz-dock tabIndex={-1} role="group" aria-label={t("Browser session actions")}>{dock}</div> : null}
      <footer className="cv-foot">
        <span>{t("session-backed")}</span>
        <span>{showFrame ? t("live view") : t("state and address only")}</span>
      </footer>
      <div className="sr-only" role="status" aria-live="polite">{note}</div>
    </div>
  );
}

/** The thread's card for the session: a way back to a hidden tab, and to the
 *  panel when nothing else opened the drawer. Absent until there is a session
 *  to speak of. */
export function BrowserCard() {
  const t = useTranslate();
  const b = useBrowserTab();
  const canvas = useCanvas();
  if (b.availability !== "available") return null;
  const phase = b.session?.state ?? "none";
  if (phase === "none" && !b.loadError) return null;
  const st = browserStatus(b, t);
  const shown = ((canvas.open && canvas.tabs.length > 0) || b.open) && b.focused && tabVisible(b);
  const sub =
    b.loadError ? t("Can't load the browser state")
    : phase === "handoff" ? `${t("Waiting for you")}${b.session?.url ? ` · ${b.session.url}` : ""}`
    : phase === "ended" ? t("Session ended")
    : (b.session?.url ?? st.text);
  return (
    <div className="bz-card-wrap">
    <button type="button" className="artcard bz-card" data-kind="session" data-open={shown || undefined} aria-label={`${t("Open")} ${t("Browser")}`} onClick={openBrowser}>
      <span className="arttype"><GlobeIcon size={16} /></span>
      <span className="arttext">
        <b>{t("Browser")}</b>
        <span className="artmeta">{sub}</span>
      </span>
      {shown ? <span className="artbadge">{t("in canvas")}</span> : <span className="artopen">{t("Open")}</span>}
    </button>
    </div>
  );
}
