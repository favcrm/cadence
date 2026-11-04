import { useEffect, useRef, useState, type FocusEvent } from "react";
import { useTranslate } from "../../lib/locale";
import { IconClose } from "../../ui/icons";
import { useCanvas } from "./canvasStore";
import { EmptyState } from "../app-shell/shared/States";
import { viewBrowser } from "./browserClient";
import {
  alertBrowser,
  cancelStop,
  confirmStop,
  openBrowser,
  browserSnapshot,
  refreshBrowser,
  requestStop,
  tabVisible,
  useBrowserTab,
  type BrowserTabState,
} from "./browserStore";

/** How long a live view may take to load before it is given up on. */
const FRAME_WAIT_MS = 20000;

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

/** The one word for the tab, header and card. */
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
 *  what the URL parser reports (punycode stays punycode, never the userinfo a
 *  link can hide a look-alike host behind); nothing is matched against it. An
 *  address that does not parse is shown whole. */
function addressParts(address: string): { scheme: string; host: string; rest: string } | null {
  try {
    const u = new URL(address);
    return { scheme: `${u.protocol}//`, host: u.host, rest: `${u.pathname}${u.search}` };
  } catch {
    return null;
  }
}

/** The last page the assistant read, inline in the header and outside the live
 *  view only: inside it the provider's own address bar is the one that matters.
 *  One line. The path gives way first; a host that still does not fit is clipped
 *  from its START, so the registrable end stays visible. */
function Address({ url }: { url: string }) {
  const t = useTranslate();
  const parts = addressParts(url);
  return (
    <div className="bz-addr" aria-label={t("Page address")} title={parts ? `${parts.scheme}${parts.host}${parts.rest}` : url}>
      {parts ? (
        <>
          <b className="bz-host"><bdi>{parts.host}</bdi></b>
          {parts.rest !== "/" ? <span className="bz-rest">{parts.rest}</span> : null}
        </>
      ) : (
        <span className="bz-rest">{url}</span>
      )}
    </div>
  );
}

const Icon = ({ d, children }: { d?: string; children?: React.ReactNode }) => (
  <svg className="bz-ico" viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
    {d ? <path d={d} /> : children}
  </svg>
);

type Viewer = { phase: "idle" } | { phase: "connecting" } | { phase: "loading" | "open"; url: string; n: number };

/** The origin of a URL, or null when it does not parse. */
function originOf(value: string): string | null {
  try {
    return new URL(value).origin;
  } catch {
    return null;
  }
}

/**
 * The Browser tab's panel. Rendered while the tab exists and merely hidden
 * when another tab is active, so a live view in progress survives a tab
 * switch. The viewer URL lives only in this component's state, from the
 * person's click until the frame is torn down, and every teardown resets the
 * viewer to idle so the next open fetches a fresh URL. Failures the person
 * has to act on are toasts (`alertBrowser`); the panel itself stays plain.
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
  // A reopen asks for a new URL while the current frame stays; if it fails the
  // person keeps what they had.
  const [fetching, setFetching] = useState(false);
  const attempt = useRef(0);
  const frames = useRef(0);
  const dockRef = useRef<HTMLDivElement | null>(null);
  const root = useRef<HTMLDivElement | null>(null);
  const heading = useRef<HTMLElement | null>(null);
  // The control inside the panel that last held focus. Cleared the moment
  // focus leaves the panel on purpose; used only if that very node is removed.
  const claim = useRef<HTMLElement | null>(null);
  const st = browserStatus(b, t);
  const phase = b.session?.state ?? "none";
  const live = phase === "active" || phase === "handoff";
  const stopping = b.stop === "stopping";
  const viewing = viewer.phase === "loading" || viewer.phase === "open";
  // A takeover over tears a frame that is still loading down, and so does any
  // other phase. A stop in flight does not: the view stays (inert) until the stop
  // has succeeded. A frame already open when the person presses Done stays, view
  // only (inert), while the assistant reads on: the same node, never reloaded.
  const viewOnly = phase === "active" && viewer.phase === "open" && !fetching;
  const showFrame = viewing && (phase === "handoff" || viewOnly);
  const asking = phase === "handoff" && !stopping && !showFrame;
  const loadingUrl = viewer.phase === "loading" ? viewer.url : null;
  const instructions = b.session?.instructions ?? "";

  useEffect(() => {
    if (phase !== "handoff" && !viewOnly && (viewer.phase !== "idle" || fetching)) {
      attempt.current++;
      setFetching(false);
      setViewer({ phase: "idle" });
    }
  }, [phase, viewer.phase, fetching, viewOnly]);

  // Focus inside the frame goes with the takeover: it moves to the tab, or the
  // heading, never onto Stop, and only if the person is still where the view was.
  const box = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!viewOnly) return;
    const at = document.activeElement;
    if (at !== document.body && !(at && box.current?.contains(at))) return;
    claim.current = null;
    (document.getElementById("bz-tab") ?? heading.current)?.focus();
  }, [viewOnly]);

  // The frame is refused by the board's own CSP: say so now. Only an enforced
  // `frame-src` violation for the viewer's own origin counts. (A frame refused
  // by the provider's X-Frame-Options or frame-ancestors, or one that answers
  // with an error page, still fires `load`: nothing here can tell.)
  useEffect(() => {
    if (loadingUrl === null) return;
    const viewerOrigin = originOf(loadingUrl);
    const giveUp = () => {
      setViewer({ phase: "idle" });
      alertBrowser("Couldn't load the live view. Check your connection, or it may not be allowed on this board yet.");
    };
    const onViolation = (e: Event) => {
      const ev = e as Event & { effectiveDirective?: string; disposition?: string; blockedURI?: string };
      if (ev.disposition === "report" || ev.effectiveDirective !== "frame-src") return;
      if (viewerOrigin === null || originOf(ev.blockedURI ?? "") !== viewerOrigin) return;
      giveUp();
    };
    document.addEventListener("securitypolicyviolation", onViolation);
    // A frame that never answers is reported, not left spinning.
    const id = setTimeout(giveUp, FRAME_WAIT_MS);
    return () => {
      document.removeEventListener("securitypolicyviolation", onViolation);
      clearTimeout(id);
    };
  }, [loadingUrl]);

  // The control that held focus was removed (Stop, a refused frame): focus
  // must not fall to the page. It goes to the retry on a failure, and
  // otherwise to the tab, or to the panel's heading when the strip has no tab.
  // Once, and never when the person has moved on: a focus that left the panel
  // cleared the claim, and a focus that is still somewhere is left alone.
  useEffect(() => {
    const was = claim.current;
    if (!active || was === null || was.isConnected || document.activeElement !== document.body) return;
    claim.current = null;
    const specific =
      b.stop === "failed" ? '[data-bz-act="stop"]'
      : phase === "handoff" && !viewing ? '[data-bz-act="view"]'
      : null;
    (specific ? root.current?.querySelector<HTMLElement>(specific) : null)?.focus();
    if (document.activeElement === document.body) (document.getElementById("bz-tab") ?? heading.current)?.focus();
  });

  // A fast 503: the focus the stop left on the dock (or nowhere) goes to the retry.
  useEffect(() => {
    if (b.stop !== "failed") return;
    const at = document.activeElement;
    // Nowhere, the dock, or the places the rescue parks focus while a stop is
    // in flight (the heading or the tab): all of them are the panel's, not the person's choice.
    if (at === document.body || at === dockRef.current || at === heading.current || at === document.getElementById("bz-tab")) {
      root.current?.querySelector<HTMLElement>('[data-bz-act="stop"]')?.focus();
    }
  }, [b.stop]);

  const onFocusIn = (e: FocusEvent<HTMLElement>) => { claim.current = e.target; };
  const onFocusOut = (e: FocusEvent<HTMLElement>) => {
    const to = e.relatedTarget as Node | null;
    // To nothing (a click on bare page) or to anything outside the panel.
    if ((to === null && e.target.isConnected) || (to !== null && !e.currentTarget.contains(to))) claim.current = null;
  };

  // Focus a control in the panel, but only if the person's focus is still in
  // the panel (or nowhere): a late answer must never pull keystrokes away from
  // the chat draft, least of all into the provider's page.
  const focusSoon = (selector: string) => requestAnimationFrame(() => {
    const at = document.activeElement;
    if (at !== document.body && !root.current?.contains(at)) return;
    root.current?.querySelector<HTMLElement>(selector)?.focus();
  });

  const openView = async () => {
    const mine = ++attempt.current;
    const had = viewing;
    if (had) setFetching(true);
    else setViewer({ phase: "connecting" });
    try {
      const r = await viewBrowser(conversation);
      if (mine !== attempt.current) return;
      setFetching(false);
      if (r.kind === "not_in_handoff") {
        // The takeover is over: the read that follows says so.
        setViewer({ phase: "idle" });
        void refreshBrowser();
        return;
      }
      setViewer({ phase: "loading", url: r.url, n: ++frames.current });
      // Into the frame only if the person is still where they clicked (the
      // button, or nowhere) and nothing else has been opened since: a stop
      // confirm or the chat draft keeps the keystrokes.
      requestAnimationFrame(() => {
        const at = document.activeElement;
        if (at !== document.body && at?.getAttribute("data-bz-act") !== "view") return;
        if (browserSnapshot().stop !== "idle") return;
        root.current?.querySelector<HTMLElement>("[data-bz-frame]")?.focus();
      });
    } catch {
      if (mine !== attempt.current) return;
      setFetching(false);
      // A failed reopen leaves the frame the person already has.
      if (!had) setViewer({ phase: "idle" });
      alertBrowser("Couldn't open the live view.");
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

  // Distinct keys: React must never reuse the Open live view node as Stop
  // session when a takeover ends under the person's focus.
  const stopBtn = (
    <button key="stop" type="button" className="btn btn-sm btn-danger" data-bz-act="stop" aria-label={t("Stop session")} title={t("Stop session")} onClick={b.stop === "failed" ? stop : ask}>
      <Icon><rect x="6" y="6" width="12" height="12" rx="1.5" /></Icon>
      <span className="bz-lbl">{t("Stop session")}</span>
    </button>
  );
  const wait = viewer.phase === "connecting" || fetching;
  const onView = () => { if (!wait) void openView(); };
  const spin = wait ? <span className="btn-spin" aria-hidden /> : null;

  let dock: React.ReactNode = null;
  if (b.stop === "confirm") {
    dock = (
      <>
        <span className="bz-ask" role="alert">{t("Stop the session?")}{b.session?.signedIn ? ` ${t("Anything signed in to this session is lost.")}` : ""}</span>
        <button key="go" type="button" className="btn btn-sm btn-danger" data-bz-act="go" onClick={stop}>{t("Stop session")}</button>
        <button key="keep" type="button" className="btn btn-sm" data-bz-act="keep" onClick={keep}>{t("Keep it running")}</button>
      </>
    );
  } else if (stopping) {
    dock = (
      <button key="stopping" type="button" className="btn btn-sm btn-danger" disabled aria-disabled="true" aria-busy="true" aria-label={t("Stopping…")}>
        <span className="btn-spin" aria-hidden />
        <span className="bz-lbl">{t("Stopping…")}</span>
      </button>
    );
  } else if (live) {
    dock = (
      <>
        {phase === "handoff" && viewing ? (
          <button key="view" type="button" className="cv-iconbtn" data-bz-act="view" aria-disabled={wait} aria-label={t("Open live view again")} title={t("Open live view again")} onClick={onView}>
            {spin ?? <Icon d="M20 12a8 8 0 1 1-2.3-5.7M20 4v5h-5" />}
          </button>
        ) : phase === "handoff" ? (
          <button key="view" type="button" className="btn btn-sm btn-primary" data-bz-act="view" aria-disabled={wait} aria-label={t("Open live view")} title={t("Open live view")} onClick={onView}>
            {spin ?? <Icon d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5" />}
            <span className="bz-lbl">{t("Open live view")}</span>
          </button>
        ) : null}
        {stopBtn}
      </>
    );
  }

  return (
    <div
      className="bz-panel"
      role="tabpanel"
      id="bz-panel"
      aria-labelledby="bz-heading"
      hidden={!active}
      ref={root}
      onKeyDown={onKeyDown}
      onFocus={onFocusIn}
      onBlur={onFocusOut}
    >
      <header className="cv-head bz-head" data-confirm={b.stop === "confirm" || undefined}>
        {sheet ? (
          <button type="button" className="cv-iconbtn cv-back" onClick={onClose} aria-label={t("Back to chat")}>
            <svg viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="m15 18-6-6 6-6" />
            </svg>
          </button>
        ) : null}
        <span className="bz-globe"><GlobeIcon size={14} /></span>
        <span className="bz-title">
          <b id="bz-heading" tabIndex={-1} ref={heading}>{t("Browser")}</b>
          <span className="bz-badge" style={{ "--c": TONE[st.tone] } as React.CSSProperties} role="status">{st.text}</span>
        </span>
        {live && !showFrame && b.session?.url ? <Address url={b.session.url} /> : <span className="bz-gap" />}
        {dock ? <div className="bz-actions" ref={dockRef} data-bz-dock tabIndex={-1} role="group" aria-label={t("Browser session actions")}>{dock}</div> : null}
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
      {/* Fixed slots, whatever the rest shows (stopping, a failed stop, a failed
          read): React must never remount the frame, which would load the bearer
          URL again. The frame is inert while the session is being stopped:
          nothing typed into a view that is going away reaches the page. */}
      <div className="cv-body bz-body" data-canvas-body data-frame={showFrame || undefined}>
        {!live ? <EmptyState title={phase === "ended" ? t("Session ended") : t("No browser session")} /> : null}
        {asking && instructions ? (
          <div className="bz-says">
            <small>{t("Your assistant:")}</small>
            <p id="bz-says-t">{instructions}</p>
          </div>
        ) : null}
        {showFrame && (viewer.phase === "loading" || viewer.phase === "open") ? (
          <div className="bz-viewer" ref={box} data-phase={viewer.phase} inert={stopping || viewOnly || undefined}>
            <iframe
              key={viewer.n}
              data-bz-frame
              title={t("Live view")}
              src={viewer.url}
              // The provider's page needs its own scripts and forms; it is a
              // different origin (the client refuses the board's own), so none
              // of this reaches the board.
              sandbox="allow-scripts allow-same-origin allow-forms"
              referrerPolicy="no-referrer"
              onLoad={() => setViewer((v) => (v.phase === "loading" ? { phase: "open", url: v.url, n: v.n } : v))}
            />
          </div>
        ) : null}
        {/* Outside the inert box, which assistive tech skips. */}
        {viewOnly ? <span className="sr-only" role="status">{t("Live view, view only while the assistant reads.")}</span> : null}
      </div>
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
  if (phase === "none") return null;
  const shown = ((canvas.open && canvas.tabs.length > 0) || b.open) && b.focused && tabVisible(b);
  // Only the real host, never the raw address (a look-alike can hide behind
  // userinfo), on a line of its own and never preceded by words that could push
  // its end out of the box: when it is too long, the START is what is clipped.
  const host = (phase === "active" || phase === "handoff") && b.session?.url ? (addressParts(b.session.url)?.host ?? "") : "";
  return (
    <div className="bz-card-wrap">
      <button type="button" className="artcard bz-card" data-kind="session" data-open={shown || undefined} aria-label={`${t("Open")} ${t("Browser")}`} onClick={openBrowser}>
        <span className="arttype"><GlobeIcon size={16} /></span>
        <span className="arttext">
          <b>{t("Browser")}</b>
          <span className="artmeta">{browserStatus(b, t).text}</span>
          {host ? <span className="artmeta bz-card-host" title={host}><span>{host}</span></span> : null}
        </span>
      </button>
    </div>
  );
}
